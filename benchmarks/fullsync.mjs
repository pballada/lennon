// Benchmark-only macOS shim. Never imported by Lennon's production code.
import { dlopen, FFIType } from 'bun:ffi';
const lib = dlopen('/usr/lib/libSystem.B.dylib', {
  fcntl: { args: [FFIType.i32, FFIType.i32], returns: FFIType.i32 },
});
export function sync(fd) {
  if (lib.symbols.fcntl(fd, 51 /* F_FULLFSYNC, Darwin fcntl.h */) !== 0) {
    throw new Error('Benchmark F_FULLFSYNC failed; refusing a weaker fallback');
  }
}
