/// <reference types="vite/client" />

// 让 `import.meta.env` 在 TS 里有类型。
//
// 目前只用到 `import.meta.env.DEV`：`main.tsx` 用它判断是否挂载浏览器预览桥接
// （见 `src/preview.ts`）。少了这个引用，`tsc` 会报
// "Property 'env' does not exist on type 'ImportMeta'"。
