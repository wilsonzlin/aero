#include <cassert>
#include <cstdint>
#include <vector>

#include "aerogpu_wddm_alloc_list.h"

namespace {

void test_dedup_and_write_upgrade() {
  D3DDDI_ALLOCATIONLIST list[4] = {};
  aerogpu::AllocationListTracker tracker(list, 4, 0xFFFFu);

  // alloc_id can be larger than MaxAllocationListSlotId; the tracker assigns
  // slot IDs densely and keeps alloc_id as a protocol-level value.
  auto r0 = tracker.track_buffer_read(1, 0x123456u, 0xABC);
  assert(r0.status == aerogpu::AllocRefStatus::kOk);
  assert(tracker.list_len() == 1);
  assert(list[0].hAllocation == 1);
  assert(list[0].AllocationListSlotId == 0);
  assert(list[0].WriteOperation == 0);

  // Dedup by handle.
  auto r1 = tracker.track_buffer_read(1, 0x123456u, 0xABC);
  assert(r1.status == aerogpu::AllocRefStatus::kOk);
  assert(r1.list_index == 0);
  assert(tracker.list_len() == 1);

  // Upgrade read -> write.
  auto r2 = tracker.track_render_target_write(1, 0x123456u, 0xABC);
  assert(r2.status == aerogpu::AllocRefStatus::kOk);
  assert(tracker.list_len() == 1);
  assert(list[0].WriteOperation == 1);

  // Alias by alloc_id (distinct handles pointing at the same allocation).
  auto r3 = tracker.track_buffer_read(2, 0x123456u, 0xABC);
  assert(r3.status == aerogpu::AllocRefStatus::kOk);
  assert(r3.list_index == 0);
  assert(tracker.list_len() == 1);

  // Collision by alloc_id (distinct handles pointing at different allocations).
  auto r4 = tracker.track_buffer_read(3, 0x123456u, 0xDEF);
  assert(r4.status == aerogpu::AllocRefStatus::kAllocIdCollision);
}

void test_mismatch_and_capacity() {
  D3DDDI_ALLOCATIONLIST list[2] = {};
  aerogpu::AllocationListTracker tracker(list, 2, 0xFFFFu);

  auto ok = tracker.track_texture_read(100, 1, 0 /*share_token*/);
  assert(ok.status == aerogpu::AllocRefStatus::kOk);
  assert(list[0].AllocationListSlotId == 0);

  auto mismatch = tracker.track_texture_read(100, 2, 0 /*share_token*/);
  assert(mismatch.status == aerogpu::AllocRefStatus::kAllocIdMismatch);

  auto ok2 = tracker.track_texture_read(200, 2, 0 /*share_token*/);
  assert(ok2.status == aerogpu::AllocRefStatus::kOk);
  assert(tracker.list_len() == 2);
  assert(list[1].AllocationListSlotId == 1);

  auto need_flush = tracker.track_texture_read(300, 3, 0 /*share_token*/);
  assert(need_flush.status == aerogpu::AllocRefStatus::kNeedFlush);
}

void test_snapshot_and_replay() {
  D3DDDI_ALLOCATIONLIST list0[4] = {};
  aerogpu::AllocationListTracker tracker(list0, 4, 0xFFFFu);

  auto r0 = tracker.track_buffer_read(1, 10, 111);
  assert(r0.status == aerogpu::AllocRefStatus::kOk);
  auto r1 = tracker.track_render_target_write(2, 20, 222);
  assert(r1.status == aerogpu::AllocRefStatus::kOk);
  auto r2 = tracker.track_buffer_read(3, 30, 333);
  assert(r2.status == aerogpu::AllocRefStatus::kOk);

  // Upgrade entry0 read -> write.
  auto r0w = tracker.track_render_target_write(1, 10, 111);
  assert(r0w.status == aerogpu::AllocRefStatus::kOk);
  assert(list0[0].WriteOperation == 1);

  // Alias by alloc_id should not create a new allocation-list entry.
  auto alias = tracker.track_buffer_read(4, 20, 222);
  assert(alias.status == aerogpu::AllocRefStatus::kOk);
  assert(tracker.list_len() == 3);

  const std::vector<aerogpu::AllocationListTracker::TrackedAllocation> snap = tracker.snapshot_tracked_allocations();
  assert(snap.size() == 3);
  assert(snap[0].hAllocation == 1);
  assert(snap[0].alloc_id == 10);
  assert(snap[0].share_token == 111);
  assert(snap[0].write);
  assert(snap[1].hAllocation == 2);
  assert(snap[1].alloc_id == 20);
  assert(snap[1].share_token == 222);
  assert(snap[1].write);
  assert(snap[2].hAllocation == 3);
  assert(snap[2].alloc_id == 30);
  assert(snap[2].share_token == 333);
  assert(!snap[2].write);

  D3DDDI_ALLOCATIONLIST list1[4] = {};
  tracker.rebind(list1, 4, 0xFFFFu);
  assert(tracker.list_len() == 0);
  assert(tracker.replay_tracked_allocations(snap));
  assert(tracker.list_len() == 3);
  assert(list1[0].hAllocation == 1);
  assert(list1[0].WriteOperation == 1);
  assert(list1[1].hAllocation == 2);
  assert(list1[1].WriteOperation == 1);
  assert(list1[2].hAllocation == 3);
  assert(list1[2].WriteOperation == 0);

  // Replay should fail if the target allocation list is too small.
  D3DDDI_ALLOCATIONLIST list2[2] = {};
  tracker.rebind(list2, 2, 0xFFFFu);
  assert(!tracker.replay_tracked_allocations(snap));
}

} // namespace

int main() {
  test_dedup_and_write_upgrade();
  test_mismatch_and_capacity();
  test_snapshot_and_replay();
  return 0;
}
