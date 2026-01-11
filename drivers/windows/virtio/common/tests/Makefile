CC ?= cc

CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2

TARGETS := virtqueue_split_test virtqueue_split_stress_test virtio_sg_pfn_test

INCLUDES := -I..

all: $(TARGETS)

virtqueue_split_test: virtqueue_split_test.c ../virtqueue_split.c
	$(CC) $(CFLAGS) $(INCLUDES) -o $@ $^

virtqueue_split_stress_test: CFLAGS += -DVIRTQ_DEBUG
virtqueue_split_stress_test: virtqueue_split_stress_test.c ../virtqueue_split.c
	$(CC) $(CFLAGS) $(INCLUDES) -o $@ $^

virtio_sg_pfn_test: virtio_sg_pfn_test.c ../virtio_sg_pfn.c
	$(CC) $(CFLAGS) $(INCLUDES) -o $@ $^

test: $(TARGETS)
	./virtqueue_split_test
	./virtqueue_split_stress_test
	./virtio_sg_pfn_test

clean:
	rm -f $(TARGETS)

.PHONY: all test clean
