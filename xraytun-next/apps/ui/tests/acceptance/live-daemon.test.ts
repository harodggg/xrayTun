// @vitest-environment jsdom
//
// 活体验收（ux / task-3）—— 真 daemon + 真 xray + 真 AF_UNIX + 真 SOCKS + 真字节。
//
// 门控：只有 `XT_LIVE=1` 时才运行。其余情况整个文件被 skip（不是「通过」）。
//
// 跑法（见 docs/ux/LIVE-EVIDENCE.md，含真实执行记录）：
//   export RUSTUP_HOME=... CARGO_HOME=... PATH="$CARGO_HOME/bin:$PATH"
//   export CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-targets/ux
//   cargo build -p xt-daemon
//   cd apps/ui && npm install
//   XT_LIVE=1 XT_SOCKET=/tmp/xraytun-live.sock npm test -- tests/acceptance/live-daemon.test.ts
//
// 本测试自己负责起「环回真 xray 服务端 + 本地 HTTP 源站 + 订阅文件 + 真 xt-daemon」，
// 然后把真 App 渲染出来、用 UI 点连接，最后经 daemon 的 SOCKS 入站发一次真实请求，
// 断言界面显示的是 StatsService 采到的真实字节数。
//
// I1：没有 sleep、没有轮询。所有等待都是「事件 / 路径出现 / vitest 超时」驱动：
//   * 等 socket 出现 → fs.watch
//   * 等 xray 起来 → 监听它的 stdout/stderr 行
//   * 等界面状态 → @testing-library 的 waitFor / findBy
//   * 失败上限 → vitest 的 test timeout（不是我们自己的定时器）

import { spawn } from 'node:child_process';
import type { ChildProcess } from 'node:child_process';
import { Fragment, createElement } from 'react';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { App } from '../../src/App';
import { DaemonProvider, useDaemon } from '../../src/store/daemon';
import { createUnixSocketClient } from '../../src/transport/unixSocket';
import type { DaemonClient } from '../../src/transport/client';

const LIVE = process.env.XT_LIVE === '1';

/**
 * 渲染探针：每次 Provider 状态变化真正 render 时记录一次 stage。
 * 为什么不用 MutationObserver：jsdom 里 MutationObserver 的回调是异步派发的，
 * waitFor 命中后立刻 disconnect 会把还没派发的记录丢掉 —— 那是测试的假象，不是界面的行为。
 * 探针记录的是 React 实际提交的渲染，确定性更好。
 */
function StageProbe({ record }: { record: string[] }): null {
  const { connection } = useDaemon();
  const stage = connection?.stage ?? 'unknown';
  if (record[record.length - 1] !== stage) record.push(stage);
  return null;
}


// 仓库根：不能用 import.meta.url —— 本文件跑在 jsdom 下（活体测试要渲染真 App），
// 而 jsdom 里 import.meta.url 不是 file: 协议，fileURLToPath 会直接抛。
// 约定从 apps/ui 运行（见文件顶部跑法），必要时用 XT_REPO_ROOT 覆盖。
const REPO_UNIT = process.env.XT_REPO_ROOT ?? path.resolve(process.cwd(), '..', '..');
const XT_SOCKET = process.env.XT_SOCKET ?? path.join(os.tmpdir(), `xraytun-live-${process.pid}.sock`);
const XT_XRAY_BIN =
  process.env.XT_XRAY_BIN ?? '/Users/xbtg-/deepseek-harness/.scratch/bin/xray';
const XT_DAEMON_BIN =
  process.env.XT_DAEMON_BIN ??
  path.join(process.env.CARGO_TARGET_DIR ?? path.join(REPO_UNIT, 'target'), 'debug', 'xt-daemon');

afterEach(cleanup);

// ------------------------------------------------------------------ 小工具（全部事件驱动）

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port = typeof address === 'object' && address !== null ? address.port : 0;
      server.close(() => resolve(port));
    });
  });
}

/** 等一个文件出现。事件驱动：fs.watch + 起始时的一次同步检查。没有轮询、没有 sleep。 */
function waitForPath(target: string): Promise<void> {
  if (fs.existsSync(target)) return Promise.resolve();
  return new Promise((resolve) => {
    const watcher = fs.watch(path.dirname(target), () => {
      if (fs.existsSync(target)) {
        watcher.close();
        resolve();
      }
    });
  });
}

/** 等子进程输出里出现匹配行（xray 就绪）。同样没有 sleep，超时由 vitest 兜底。 */
function waitForLine(proc: ChildProcess, pattern: RegExp, label: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const onData = (chunk: Buffer) => {
      const text = chunk.toString();
      if (pattern.test(text)) {
        cleanupListeners();
        resolve(text.trim().split('\n').find((line) => pattern.test(line)) ?? text.trim());
      }
    };
    const onExit = (code: number | null) => {
      cleanupListeners();
      reject(new Error(`${label} 在就绪前退出，code=${code}`));
    };
    const cleanupListeners = () => {
      proc.stdout?.off('data', onData);
      proc.stderr?.off('data', onData);
      proc.off('exit', onExit);
    };
    proc.stdout?.on('data', onData);
    proc.stderr?.on('data', onData);
    proc.on('exit', onExit);
  });
}

/**
 * 最小 SOCKS5 客户端：CONNECT + HTTP GET，返回响应体。
 * 手写而不是引第三方包：依赖越小，活体验收的失败就越能归因到被测路径。
 */
function socksHttpGet(socksPort: number, host: string, port: number, urlPath: string): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const socket = net.connect(socksPort, '127.0.0.1');
    let stage: 'greet' | 'reply' | 'body' = 'greet';
    let buffer = Buffer.alloc(0);
    let body = Buffer.alloc(0);

    socket.on('connect', () => socket.write(Buffer.from([0x05, 0x01, 0x00])));
    socket.on('error', reject);
    socket.on('end', () => resolve(body));
    socket.on('data', (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (stage === 'greet') {
        if (buffer.length < 2) return;
        if (buffer[0] !== 0x05 || buffer[1] !== 0x00) {
          reject(new Error(`SOCKS 方法协商失败：${buffer.subarray(0, 2).toString('hex')}`));
          return;
        }
        buffer = buffer.subarray(2);
        const hostBytes = Buffer.from(host, 'utf8');
        socket.write(
          Buffer.concat([
            Buffer.from([0x05, 0x01, 0x00, 0x03, hostBytes.length]),
            hostBytes,
            Buffer.from([(port >> 8) & 0xff, port & 0xff]),
          ]),
        );
        stage = 'reply';
      }
      if (stage === 'reply') {
        if (buffer.length < 4) return;
        const atyp = buffer[3];
        const addrLen = atyp === 0x01 ? 4 : atyp === 0x04 ? 16 : atyp === 0x03 ? 1 + buffer[4]! : -1;
        if (addrLen < 0) {
          reject(new Error(`SOCKS 应答 atyp 未知：${atyp}`));
          return;
        }
        const total = 4 + addrLen + 2;
        if (buffer.length < total) return;
        if (buffer[1] !== 0x00) {
          reject(new Error(`SOCKS CONNECT 被拒绝，reply=${buffer[1]}`));
          return;
        }
        buffer = buffer.subarray(total);
        socket.write(
          `GET ${urlPath} HTTP/1.1\r\nHost: ${host}\r\nConnection: close\r\n\r\n`,
        );
        stage = 'body';
      }
      if (stage === 'body') {
        body = Buffer.concat([body, buffer]);
        buffer = Buffer.alloc(0);
      }
    });
  });
}

/** 从 HTTP 响应里取出 body（我们自己的源站，格式已知）。 */
function httpBody(response: Buffer): Buffer {
  const marker = response.indexOf('\r\n\r\n');
  return marker < 0 ? Buffer.alloc(0) : response.subarray(marker + 4);
}

const VLESS_UUID = 'b831381d-6324-4d53-ad4f-8cda48b30811'; // 测试用固定 UUID，无机密含义
const ORIGIN_BYTES = 64 * 1024;

interface LiveRig {
  daemon?: ReturnType<typeof spawn>;
  xrayServer?: ReturnType<typeof spawn>;
  origin?: http.Server;
  client?: DaemonClient;
  tmpDir: string;
}

describe.skipIf(!LIVE)('live: 真 daemon + 真 xray → 事件流驱动 UI 到已连接并显示真字节', () => {
  const rig: LiveRig = { tmpDir: '' };

  afterEach(async () => {
    try {
      rig.client?.close();
    } catch {
      /* 清理失败不掩盖断言结果 */
    }
    rig.daemon?.kill('SIGKILL');
    rig.xrayServer?.kill('SIGKILL');
    if (rig.origin !== undefined) {
      await new Promise<void>((resolve) => rig.origin!.close(() => resolve()));
    }
    if (rig.tmpDir !== '') fs.rmSync(rig.tmpDir, { recursive: true, force: true });
  });

  it(
    '连接 → SOCKS 真流量 → stats 真字节 → 断开',
    async () => {
      // 失败时把两个子进程的真实输出带进断言信息里：活体失败最怕「只看到 EPIPE」。
      let daemonLog = '';
      let xrayLog = '';
      let step = 'init';
      // 整个活体流程包一层：失败时把子进程真实输出附在错误里（只看到 EPIPE 无法归因）。
      try {
      // 前置条件必须真实存在：缺任何一个都直接失败，而不是静默跳过。
      expect(fs.existsSync(XT_XRAY_BIN), `真 xray 不存在：${XT_XRAY_BIN}`).toBe(true);
      expect(fs.existsSync(XT_DAEMON_BIN), `xt-daemon 未构建：先 cargo build -p xt-daemon（${XT_DAEMON_BIN}）`).toBe(true);

      rig.tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'xraytun-live-'));
      const originPort = await freePort();
      const xrayPort = await freePort();
      const socksPort = await freePort();

      // 1) 本地 HTTP 源站（真 socket、真字节）
      const originBody = Buffer.alloc(ORIGIN_BYTES, 0x41);
      rig.origin = http.createServer((_req, res) => {
        res.writeHead(200, {
          'content-type': 'application/octet-stream',
          'content-length': String(originBody.length),
        });
        res.end(originBody);
      });
      await new Promise<void>((resolve) => rig.origin!.listen(originPort, '127.0.0.1', resolve));

      // 2) 环回真 xray 服务端（vless inbound + freedom outbound），配置由 backend-3 提供
      const serverConfigPath = path.join(rig.tmpDir, 'xray-server.json');
      fs.writeFileSync(
        serverConfigPath,
        JSON.stringify({
          log: { loglevel: 'warning' },
          inbounds: [
            {
              tag: 'vless-in',
              listen: '127.0.0.1',
              port: xrayPort,
              protocol: 'vless',
              settings: { clients: [{ id: VLESS_UUID }], decryption: 'none' },
              streamSettings: { network: 'tcp' },
            },
          ],
          outbounds: [{ tag: 'freedom-out', protocol: 'freedom' }],
        }),
      );
      rig.xrayServer = spawn(XT_XRAY_BIN, ['run', '-c', serverConfigPath], { stdio: ['ignore', 'pipe', 'pipe'] });
      rig.xrayServer.stdout?.on('data', (c: Buffer) => (xrayLog += c.toString()));
      rig.xrayServer.stderr?.on('data', (c: Buffer) => (xrayLog += c.toString()));
      step = 'xray-started';
      await waitForLine(rig.xrayServer, /started/i, 'xray 服务端');

      // 3) 订阅文件：daemon 启动时读取（本轮远端拉取不做，这是唯一造节点路径）
      const subscriptionPath = path.join(rig.tmpDir, 'subscription.txt');
      fs.writeFileSync(
        subscriptionPath,
        `vless://${VLESS_UUID}@127.0.0.1:${xrayPort}?encryption=none&type=tcp&security=none#live-node\n`,
      );

      // 4) 真 xt-daemon
      const stateDir = path.join(rig.tmpDir, 'state');
      fs.mkdirSync(stateDir);
      try {
        fs.rmSync(XT_SOCKET, { force: true });
      } catch {
        /* 旧 socket 清不掉就让 bind 去报错，不掩盖 */
      }
      rig.daemon = spawn(
        XT_DAEMON_BIN,
        [
          '--socket',
          XT_SOCKET,
          '--state-dir',
          stateDir,
          '--xray',
          XT_XRAY_BIN,
          '--log-level',
          'debug',
          '--subscription-file',
          subscriptionPath,
        ],
        { stdio: ['ignore', 'pipe', 'pipe'] },
      );
      daemonLog = '';
      rig.daemon.stdout?.on('data', (c: Buffer) => (daemonLog += c.toString()));
      rig.daemon.stderr?.on('data', (c: Buffer) => (daemonLog += c.toString()));

      step = 'socket-present';
      await waitForPath(XT_SOCKET);

      // 5) 真 AF_UNIX 客户端（frontend 的 unixSocket.ts）
      const client = createUnixSocketClient(XT_SOCKET);
      rig.client = client;

      // 6) 驱动真 UI 渲染
      const stageRenders: string[] = [];
      render(
        createElement(
          DaemonProvider,
          { client },
          createElement(Fragment, null, createElement(StageProbe, { record: stageRenders }), createElement(App)),
        ),
      );
      await waitFor(() => expect(screen.getByTestId('stage-badge').textContent).toBe('未连接'), {
        timeout: 15_000,
      });

      // 7) 记录真实事件流（事件驱动的唯一判据）
      const eventStages: string[] = [];
      const offStageRecord = client.onEvent((event) => {
        if (event.event === 'state') eventStages.push(event.view.stage);
      });

      // 8) 真节点目录来自订阅文件
      step = 'ui-disconnected';
      const nodes = await client.listNodes();
      expect(nodes.length, `订阅文件未解析出节点；daemon 日志：\n${daemonLog}`).toBeGreaterThan(0);

      // 9) SOCKS 入站改到一个空闲端口（默认 1080 可能被占；改完重连生效 → 必须在 connect 之前）
      await client.patchSettings({ socks_listen: `127.0.0.1:${socksPort}` });
      const settings = await client.getSettings();
      expect(settings.socks_listen).toBe(`127.0.0.1:${socksPort}`);

      // 10) 经 UI 选节点（走 patchSettings.selected_node）→ 回仪表盘 → 经 UI 点连接
      fireEvent.click(screen.getByTestId('nav-nodes'));
      const selectButton = await screen.findByTestId('select-button', {}, { timeout: 15_000 });
      fireEvent.click(selectButton);
      await waitFor(() => expect((screen.getByTestId('select-button') as HTMLButtonElement).disabled).toBe(true));

      fireEvent.click(screen.getByTestId('nav-dashboard'));
      const connectButton = screen.getByTestId('connect-button') as HTMLButtonElement;
      await waitFor(() => expect(connectButton.disabled).toBe(false), { timeout: 15_000 });
      fireEvent.click(connectButton);

      await waitFor(() => expect(screen.getByTestId('stage-badge').textContent).toBe('已连接'), {
        timeout: 30_000,
      });
      // 事件流本身必须经过 connecting，且顺序是 connecting → connected（不是直接跳过去）。
      const firstConnecting = eventStages.indexOf('connecting');
      const firstConnected = eventStages.indexOf('connected');
      expect(firstConnecting, `真实事件序列：${eventStages.join(' → ')}`).toBeGreaterThanOrEqual(0);
      expect(firstConnected).toBeGreaterThan(firstConnecting);

      // 界面（React 实际提交的渲染）也确实从 disconnected 经 connecting 到 connected。
      const renderedConnecting = stageRenders.indexOf('connecting');
      const renderedConnected = stageRenders.indexOf('connected');
      expect(stageRenders[0]).toBe('unknown');
      expect(stageRenders, `界面渲染序列：${stageRenders.join(' → ')}`).toContain('disconnected');
      expect(renderedConnecting, `界面渲染序列：${stageRenders.join(' → ')}`).toBeGreaterThanOrEqual(0);
      expect(renderedConnected).toBeGreaterThan(renderedConnecting);
      // 证据：把真实观测打进测试输出，便于写 LIVE-EVIDENCE.md（不是断言，是记录）。
      console.log(`[live] 真实事件流 stage：${eventStages.join(' → ')}`);
      console.log(`[live] 界面渲染 stage：${stageRenders.join(' → ')}`);

      // 11) 经 daemon 的 SOCKS 入站发一次真实请求，确认拿到全部字节
      step = 'connected';
      const raw = await socksHttpGet(socksPort, '127.0.0.1', originPort, '/payload');
      const received = httpBody(raw);
      console.log(`[live] 经 daemon SOCKS 实际收到：${received.length} 字节（期望 ${ORIGIN_BYTES}）`);
      expect(received.length, `经 SOCKS 实际收到 ${received.length} 字节`).toBe(ORIGIN_BYTES);

      // 12) 触发一次真实采样（daemon 按需采样，没有定时器），等事件把 stats 送进 UI
      step = 'socks-request-done';
      const snapshot = await client.status();
      // 注意用 toBeDefined 而不是 not.toBeNull：`stats` 缺失是 undefined，not.toBeNull 会漏过它。
      expect(
        snapshot.stats,
        `status.stats 缺失 = 未采样（本次会话 StatsService 未连上）；daemon 日志末尾：\n${daemonLog.slice(-1500)}`,
      ).toBeDefined();
      expect(snapshot.stats!.downlink_bytes).toBeGreaterThanOrEqual(ORIGIN_BYTES);
      console.log(
        `[live] StatsService 真样本：uplink=${snapshot.stats!.uplink_bytes} downlink=${snapshot.stats!.downlink_bytes} sampled_at_ms=${snapshot.stats!.sampled_at_ms}`,
      );

      await waitFor(
        () => {
          const text = screen.getByTestId('stats-downlink').textContent ?? '';
          const numbers = (text.match(/\d+/g) ?? []).map(Number);
          expect(numbers.some((n) => n >= ORIGIN_BYTES), `界面下行文本：${text}`).toBe(true);
        },
        { timeout: 15_000 },
      );
      console.log(
        `[live] 界面 stats-downlink 文本：${screen.getByTestId('stats-downlink').textContent}`,
      );

      // 13) 经 UI 断开，事件把界面送回未连接
      fireEvent.click(screen.getByTestId('disconnect-button'));
      await waitFor(() => expect(screen.getByTestId('stage-badge').textContent).toBe('未连接'), {
        timeout: 30_000,
      });

      offStageRecord();
      } catch (error) {
        // 传输层 reject 的是契约的 ErrorBody（不是 Error），所以也要能把它打出来。
        const message =
          error instanceof Error ? error.message : JSON.stringify(error, null, 2);
        throw new Error(
          `[step=${step}] ${message}\n--- daemon exitCode=${String(rig.daemon?.exitCode)} signal=${String(rig.daemon?.signalCode)} ---\n${daemonLog || '(空)'}\n--- xray 服务端输出 ---\n${xrayLog || '(空)'}`,
        );
      }
    },
    180_000,
  );
});
