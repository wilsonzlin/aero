#include <cassert>
#include <cstdint>

#include "aerogpu_wddm_alloc_list.h"

namespace {

void test_dedup_and_write_upgrade() {
  D3DDDI_ALLOCATIONLIST list[4] = {};
  aerogpu::AllocationListTracker tracker(list, 4, 0xFFFFu);

  // alloc_id can be larger than MaxAllocationListSlotId; the tracker assigns
  // slot IDs densely and keeps alloc_id as a protocol-level value.
  auto r0 = tracker.track_buffer_read(1, 0x123456u);
  assert(r0.status == aerogpu::AllocRefStatus::kOk);
  assert(tracker.list_len() == 1);
  assert(list[0].hAllocation == 1);
  assert(list[0].AllocationListSlotId == 0);
  assert(list[0].WriteOperation == 0);

  // Dedup by handle.
  auto r1 = tracker.track_buffer_read(1, 0x123456u);
  assert(r1.status == aerogpu::AllocRefStatus::kOk);
  assert(r1.list_index == 0);
  assert(tracker.list_len() == 1);

  // Upgrade read -> write.
  auto r2 = tracker.track_render_target_write(1, 0x123456u);
  assert(r2.status == aerogpu::AllocRefStatus::kOk);
  assert(tracker.list_len() == 1);
  assert(list[0].WriteOperation == 1);

  // Alias by alloc_id (distinct handles pointing at the same allocation).
  auto r3 = tracker.track_buffer_read(2, 0x123456u);
  assert(r3.status == aerogpu::AllocRefStatus::kOk);
  assert(r3.list_index == 0);
  assert(tracker.list_len() == 1);
}

void test_mismatch_and_capacity() {
  D3DDDI_ALLOCATIONLIST list[2] = {};
  aerogpu::AllocationListTracker tracker(list, 2, 0xFFFFu);

  auto ok = tracker.track_texture_read(100, 1);
  assert(ok.status == aerogpu::AllocRefStatus::kOk);
  assert(list[0].AllocationListSlotId == 0);

  auto mismatch = tracker.track_texture_read(100, 2);
  assert(mismatch.status == aerogpu::AllocRefStatus::kAllocIdMismatch);

  auto ok2 = tracker.track_texture_read(200, 2);
  assert(ok2.status == aerogpu::AllocRefStatus::kOk);
  assert(tracker.list_len() == 2);
  assert(list[1].AllocationListSlotId == 1);

  auto need_flush = tracker.track_texture_read(300, 3);
  assert(need_flush.status == aerogpu::AllocRefStatus::kNeedFlush);
}

} // namespace

int main() {
  test_dedup_and_write_upgrade();
  test_mismatch_and_capacity();
  return 0;
}
