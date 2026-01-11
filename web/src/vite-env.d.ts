/// <reference types="vite/client" />
/// <reference types="@webgpu/types" />
/// <reference types="wicg-file-system-access" />
/// <reference types="w3c-web-hid" />

export {};

declare module '*.glsl?raw' {
  const src: string;
  export default src;
}

declare module '*.wgsl?raw' {
  const src: string;
  export default src;
}
