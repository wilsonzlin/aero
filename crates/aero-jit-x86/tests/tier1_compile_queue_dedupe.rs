use aero_cpu_core::jit::runtime::CompileRequestSink;
use aero_jit_x86::tier1::pipeline::Tier1CompileQueue;

#[test]
fn tier1_compile_queue_dedupes_and_allows_requeue_after_drain() {
    let queue = Tier1CompileQueue::new();
    let rip = 0x1000u64;

    let mut sink = queue.clone();
    sink.request_compile(rip);
    sink.request_compile(rip);
    sink.request_compile(rip);

    assert_eq!(queue.len(), 1);
    assert_eq!(queue.drain(), vec![rip]);

    // Draining clears the de-dupe set so the same RIP can be requested again (e.g. after the
    // runtime rejected a stale compilation result).
    sink.request_compile(rip);
    assert_eq!(queue.drain(), vec![rip]);
}
