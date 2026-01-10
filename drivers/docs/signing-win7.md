# Driver signing for Windows 7 (development)

## When you need this

The upstream virtio-win drivers are typically already signed for Windows. However, **custom Aero drivers** (notably the optional GPU path) will need a signing strategy.

## Test-signing (recommended for development)

Inside the Windows 7 guest (run in an elevated command prompt):

```bat
bcdedit /set testsigning on
bcdedit /set nointegritychecks off
shutdown /r /t 0
```

After reboot, Windows shows “Test Mode” in the desktop watermark and will accept drivers signed with a test certificate.

To disable:

```bat
bcdedit /set testsigning off
shutdown /r /t 0
```

## Signing a driver with a self-signed test certificate (host-side)

High-level steps (performed on a Windows build machine with the Windows SDK/WDK tools installed):

1. Create a test certificate (example using PowerShell):
   - `New-SelfSignedCertificate` (LocalMachine\My)
2. Export the certificate and import it into the guest’s Trusted Root + Trusted Publishers stores.
3. Generate a catalog (`.cat`) for the driver package.
4. Sign the catalog with `signtool sign`.

Exact commands depend on your WDK version and driver type; keep the process scripted so builds are reproducible.

