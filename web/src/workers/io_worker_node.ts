import { workerData } from "node:worker_threads";
import { runIoWorkerServer } from "./io_worker_runtime.ts";

runIoWorkerServer(workerData);

