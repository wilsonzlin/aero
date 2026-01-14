#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>

  #include "aerogpu_trace.h"

BOOL WINAPI DllMain(HINSTANCE, DWORD reason, LPVOID) {
  try {
    switch (reason) {
      case DLL_PROCESS_ATTACH:
        aerogpu::d3d9_trace_init_from_env();
        break;
      case DLL_PROCESS_DETACH:
        aerogpu::d3d9_trace_on_process_detach();
        break;
      default:
        break;
    }
  } catch (...) {
  }
  return TRUE;
}
#endif
