#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>

BOOL WINAPI DllMain(HINSTANCE, DWORD, LPVOID) {
  return TRUE;
}
#endif

