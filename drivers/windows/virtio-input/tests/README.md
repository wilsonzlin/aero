# Host-side unit tests

`hid_translate.c/h` is written to be portable so the virtio-input → HID mapping
can be validated without the Windows WDK.

Run:

```bash
gcc -std=c11 -Wall -Wextra -Werror \
  -o /tmp/hid_translate_test \
  hid_translate_test.c ../src/hid_translate.c && /tmp/hid_translate_test
```

