declare module 'tar-js' {
  interface TarAppendOptions {
    mode?: number;
    mtime?: number;
    uid?: number;
    gid?: number;
    owner?: string;
    group?: string;
  }

  class Tar {
    readonly out: Uint8Array;
    readonly written: number;
    append(filepath: string, input: string | Uint8Array, options?: TarAppendOptions): Uint8Array;
  }

  export default Tar;
}
