CC ?= cc

CFLAGS ?= -std=c11 -Wall -Wextra -Werror -O2

TARGET := virtqueue_split_test

SRCS := \
	virtqueue_split_test.c \
	../virtqueue_split.c

INCLUDES := -I..

all: $(TARGET)

$(TARGET): $(SRCS)
	$(CC) $(CFLAGS) $(INCLUDES) -o $@ $^

test: $(TARGET)
	./$(TARGET)

clean:
	rm -f $(TARGET)

.PHONY: all test clean
