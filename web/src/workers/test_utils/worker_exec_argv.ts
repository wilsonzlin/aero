import { makeNodeWorkerExecArgv } from "../../test_utils/worker_threads_exec_argv";

export const WORKER_THREADS_WEBWORKER_SHIM_URL = new URL("../test_workers/worker_threads_webworker_shim.ts", import.meta.url);

export const WORKER_THREADS_WEBWORKER_EXEC_ARGV = makeNodeWorkerExecArgv([WORKER_THREADS_WEBWORKER_SHIM_URL]);

export const NET_WORKER_NODE_SHIM_URL = new URL("../test_workers/net_worker_node_shim.ts", import.meta.url);

export const NET_WORKER_NODE_EXEC_ARGV = makeNodeWorkerExecArgv([NET_WORKER_NODE_SHIM_URL]);

export const IO_WORKER_DISK_CLIENT_SPY_SHIM_URL = new URL("../test_workers/io_worker_disk_client_spy_shim.ts", import.meta.url);

export const IO_WORKER_DISK_CLIENT_SPY_EXEC_ARGV = makeNodeWorkerExecArgv([
  WORKER_THREADS_WEBWORKER_SHIM_URL,
  IO_WORKER_DISK_CLIENT_SPY_SHIM_URL,
]);

