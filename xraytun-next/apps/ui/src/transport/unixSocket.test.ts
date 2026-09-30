// @vitest-environment node
//
// unixSocket.ts 的自检：与一个**真实** AF_UNIX 服务端（本文件内的最小 daemon）跑真帧。
//
// 为什么值得单独写：这条通道是 ux 活体验收（真 xraytund）之前唯一能证明
// 「长度前缀 + JSON 帧、请求 id 相关、seq 跳号检测、错误如实抛出」真的通了的东西。
//
// 测试数据说明：下面 mini daemon 的返回值（版本 9.9.9、pid 4242 之类）是**测试 fixture**，
// 运行时不存在这种数据源，也不允许存在。

import { mkdtemp, rm } from 'node:fs/promises';
import { createServer, type Server, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import type { DaemonClient } from './client';
import type { DaemonEvent, Frame, Request, Response } from './contract';
import { createUnixSocketClient } from './unixSocket';

// ------------------------------------------------------------------ mini daemon（测试夹具）

function encodeFrame(frame: Frame): Buffer {
  const payload = Buffer.from(JSON.stringify(frame), 'utf8');
  const header = Buffer.alloc(4);
  header.writeUInt32BE(payload.length, 0);
  return Buffer.concat([header, payload]);
}

function ok(id: number, response: Response): Frame {
  return { kind: 'response', id, outcome: { status: 'ok', response } };
}

function fail(id: number, code: 'conflict' | 'not_found', message: string): Frame {
  return { kind: 'response', id, outcome: { status: 'error', error: { code, message } } };
}

const HELLO: Response = {
  result: 'hello',
  daemon_version: '9.9.9',
  protocol_version: 1,
  capabilities: ['proxy_mode'],
  pid: 4242,
  started_at_ms: 1_000,
};

const STATUS: Response = {
  result: 'status',
  stage: 'disconnected',
  datapath: {},
};

const NODES: Response = {
  result: 'nodes',
  nodes: [{ id: 'node-1', name: '节点一', protocol: 'vless', endpoint: '127.0.0.1:443', source: { kind: 'manual' } }],
};

interface MiniDaemon {
  socketPath: string;
  /** 让服务端主动推一帧事件（测试用），返回 server 是否还活着。 */
  emit(seq: number, event: DaemonEvent): boolean;
  close(): Promise<void>;
}

async function startMiniDaemon(options: { dropSecondEvent?: boolean } = {}): Promise<MiniDaemon> {
  const dir = await mkdtemp(join(tmpdir(), 'xt-ipc-'));
  const socketPath = join(dir, 'daemon.sock');
  const connections: Socket[] = [];

  const server: Server = createServer((socket) => {
    connections.push(socket);
    let inbound = Buffer.alloc(0);
    socket.on('data', (chunk: Buffer) => {
      inbound = Buffer.concat([inbound, chunk]);
      while (inbound.length >= 4) {
        const length = inbound.readUInt32BE(0);
        if (inbound.length < 4 + length) return;
        const frame = JSON.parse(inbound.subarray(4, 4 + length).toString('utf8')) as Frame;
        inbound = inbound.subarray(4 + length);
        if (frame.kind !== 'request') continue;
        const { id } = frame;
        const request: Request = frame.request;
        switch (request.op) {
          case 'hello':
            socket.write(encodeFrame(ok(id, HELLO)));
            break;
          case 'subscribe':
            socket.write(encodeFrame(ok(id, { result: 'subscribed', topics: request.topics })));
            break;
          case 'status':
            socket.write(encodeFrame(ok(id, STATUS)));
            break;
          case 'list_nodes':
            socket.write(encodeFrame(ok(id, NODES)));
            break;
          case 'connect': {
            socket.write(encodeFrame(ok(id, { result: 'accepted' })));
            socket.write(
              encodeFrame({
                kind: 'event',
                seq: options.dropSecondEvent === true ? 2 : 1,
                event: {
                  event: 'state',
                  view: { stage: 'connecting', phase: 'starting_core', mode: 'proxy', node_id: request.node_id, datapath: {} },
                },
              }),
            );
            break;
          }
          case 'switch_node':
            socket.write(encodeFrame(fail(id, 'conflict', '未连接时不能切换节点')));
            break;
          case 'tail_logs':
            socket.write(
              encodeFrame(
                ok(id, { result: 'logs', logs: [{ ts_ms: 7, level: 'info', target: 'xt-daemon', message: '真实日志行' }] }),
              ),
            );
            break;
          default:
            socket.write(encodeFrame(fail(id, 'not_found', `测试夹具没有实现 ${request.op}`)));
        }
      }
    });
  });

  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(socketPath, () => resolve());
  });

  return {
    socketPath,
    emit(seq, event) {
      const alive = connections.filter((socket) => !socket.destroyed);
      if (alive.length === 0) return false;
      for (const socket of alive) socket.write(encodeFrame({ kind: 'event', seq, event }));
      return true;
    },
    async close() {
      for (const socket of connections) socket.destroy();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      await rm(dir, { recursive: true, force: true });
    },
  };
}

function nextEvent(client: DaemonClient): Promise<DaemonEvent> {
  return new Promise((resolve) => {
    const off = client.onEvent((event) => {
      off();
      resolve(event);
    });
  });
}

const running: MiniDaemon[] = [];
const clients: DaemonClient[] = [];

afterEach(async () => {
  for (const client of clients.splice(0)) client.close();
  for (const daemon of running.splice(0)) await daemon.close();
});

describe('unixSocket 真 AF_UNIX 传输', () => {
  it('hello → subscribe → status → listNodes → 事件 → tailLogs → ErrorBody 原样抛出', async () => {
    const daemon = await startMiniDaemon();
    running.push(daemon);
    const client = createUnixSocketClient(daemon.socketPath);
    clients.push(client);

    const hello = await client.hello();
    expect(hello.daemon_version).toBe('9.9.9');
    expect(hello.protocol_version).toBe(1);

    await client.subscribe(['state', 'log', 'probe', 'notice']);

    const status = await client.status();
    expect(status.stage).toBe('disconnected');
    expect(status.datapath).toEqual({});

    const nodes = await client.listNodes();
    expect(nodes).toHaveLength(1);
    expect(nodes[0].id).toBe('node-1');

    // connect 只受理；终态必须由事件到达（受理/终态分离）。
    const pendingEvent = nextEvent(client);
    await client.connect('node-1', 'proxy');
    const event = await pendingEvent;
    expect(event.event).toBe('state');
    // 判别式联合必须窄化后再取字段（TS 不允许跨成员读 `.view`）。
    if (event.event !== 'state') throw new Error(`期望 state 事件，收到 ${event.event}`);
    expect(event.view.stage).toBe('connecting');

    const logs = await client.tailLogs(10);
    expect(logs).toHaveLength(1);
    expect(logs[0].message).toBe('真实日志行');

    // daemon 的失败必须原样是 ErrorBody（不包装、不吞）。
    await expect(client.switchNode('node-1')).rejects.toMatchObject({ code: 'conflict', message: '未连接时不能切换节点' });
  });

  it('事件 seq 跳号 → onTransportError 收到 internal，且连接被判死', async () => {
    const daemon = await startMiniDaemon();
    running.push(daemon);
    const client = createUnixSocketClient(daemon.socketPath);
    clients.push(client);

    // 事件驱动地等这次失败，不轮询、不 sleep：handler 被调用就是「到了」。
    const transportError = new Promise<{ code: string; message: string }>((resolve) => {
      client.onTransportError?.((error) => resolve(error));
    });
    await client.hello();
    await client.subscribe(['state']);

    // 跳号：seq 1 → seq 3（2 丢了）。静默继续会让界面显示一份看起来完整的假序列。
    daemon.emit(1, { event: 'state', view: { stage: 'connecting', datapath: {} } });
    daemon.emit(3, { event: 'state', view: { stage: 'connected', datapath: {} } });

    const error = await transportError;
    expect(error.code).toBe('internal');
    expect(error.message).toContain('跳号');
    // 连接已不可信：后续请求直接失败（同一个致命错误），而不是「再试一次」。
    await expect(client.status()).rejects.toMatchObject({ code: 'internal' });
  });

  it('socket 不存在 → hello 抛 io，而不是假装连上', async () => {
    const missing = join(tmpdir(), `xt-missing-${process.pid}-${Math.floor(Math.random() * 1e6)}.sock`);
    const client = createUnixSocketClient(missing);
    clients.push(client);
    await expect(client.hello()).rejects.toMatchObject({ code: 'io' });
  });
});
