#!/usr/bin/env python3
"""Create the headless Windows VM (KVM/QEMU via libvirt) that hosts the Windows Forgejo runner.

Ports `scripts/provision-windows-vm.sh`. See RUNNERS.md for why Windows needs a *real* Windows
environment (fxc, the classic Direct3D HLSL shader compiler gpui's Windows backend shells out to at
build time, has no cross-platform build at all -- unlike dxc, the newer DXIL/SM6+ compiler, which
isn't what gpui wants here).

Run `scripts/runner-setup-linux.sh` first (installs qemu-full/libvirt) -- that one, and
`runner-setup-macos.sh`/`runner-setup-windows.ps1`, stay shell/PowerShell: they bootstrap a machine
that has no Python yet. This one runs downstream of that, so Python being available is a safe
assumption.

    provision-windows-vm.py

What you do manually (can't be scripted -- no stable, unauthenticated direct download link, and
Microsoft's evaluation center gates it behind a EULA form):
  1. Grab the Windows Server 2022 Evaluation ISO from
     https://www.microsoft.com/evalcenter/download-windows-server-2022
     (Windows Server, not Windows 11 -- no TPM/Secure Boot fuss for a headless build box, and the
     eval is the same 180-day deal either way)
  2. Save it as the path this script asks for below.
Everything else -- the VirtIO driver ISO, the VM definition, starting it -- is automatic.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

VM_NAME = "dtb-ke-windows-runner"
VM_RAM_MB = 16384
VM_VCPUS = 6
VM_DISK_GB = 120
DISK_POOL_DIR = Path("/var/lib/libvirt/images")
WIN_ISO = Path.home() / "Downloads" / "WindowsServer2022Eval.iso"
VIRTIO_ISO = DISK_POOL_DIR / "virtio-win.iso"


def run(args: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(args, **kwargs)


def main() -> int:
    for tool in ("virt-install", "virsh", "qemu-img"):
        if shutil.which(tool) is None:
            print(f"missing {tool} — run scripts/runner-setup-linux.sh first", file=sys.stderr)
            return 1

    existing = run(["virsh", "dominfo", VM_NAME], capture_output=True)
    if existing.returncode == 0:
        print(f"VM '{VM_NAME}' already exists. To start over: virsh undefine {VM_NAME} --remove-all-storage")
        print("Current state:")
        print(existing.stdout.decode())
        return 0

    if not WIN_ISO.is_file():
        print(
            f"""Windows install ISO not found at:
  {WIN_ISO}

Download the Windows Server 2022 Evaluation ISO from:
  https://www.microsoft.com/evalcenter/download-windows-server-2022

...and save it to that path (or edit WIN_ISO at the top of this script),
then re-run.""",
            file=sys.stderr,
        )
        return 1

    if not VIRTIO_ISO.is_file():
        print(
            "fetching the VirtIO Windows driver ISO (network/disk drivers the installer needs to "
            "see the virtio disk) …"
        )
        run(["sudo", "mkdir", "-p", str(DISK_POOL_DIR)], check=True)
        run(
            [
                "sudo", "curl", "-fsSL", "-o", str(VIRTIO_ISO),
                "https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso",
            ],
            check=True,
        )

    disk = DISK_POOL_DIR / f"{VM_NAME}.qcow2"
    if not disk.is_file():
        run(["sudo", "qemu-img", "create", "-f", "qcow2", str(disk), f"{VM_DISK_GB}G"], check=True)

    print("creating the VM (headless — no GPU passthrough; shader *compilation* is CPU-only, we're not rendering anything) …")
    run(
        [
            "sudo", "virt-install",
            "--name", VM_NAME,
            "--memory", str(VM_RAM_MB),
            "--vcpus", str(VM_VCPUS),
            "--cpu", "host-passthrough",
            "--os-variant", "win2k22",
            "--disk", f"path={disk},bus=virtio",
            "--disk", f"path={WIN_ISO},device=cdrom",
            "--disk", f"path={VIRTIO_ISO},device=cdrom",
            "--network", "network=default,model=virtio",
            "--graphics", "vnc,listen=0.0.0.0",
            "--video", "qxl",
            "--boot", "uefi",
            "--noautoconsole",
        ],
        check=True,
    )

    print(
        f"""
VM '{VM_NAME}' created and starting. Finish the Windows install over VNC:
  virsh domdisplay {VM_NAME}     # or: ssh -L 5900:localhost:<port> <this box>, then any VNC client

During setup, when the installer doesn't see a disk: "Load driver" →
the VirtIO CD → viostor\\<your Windows version>\\amd64.

Once Windows is installed and you can log in:
  1. Install the VirtIO Guest Tools from the same VirtIO CD (network driver,
     QEMU guest agent — makes 'virsh shutdown' etc. work cleanly).
  2. Enable an SSH server or RDP for headless access from here on (Windows
     Server: Server Manager → Add Roles → OpenSSH Server, or
     'Add-WindowsCapability -Online -Name OpenSSH.Server').
  3. Copy scripts/runner-setup-windows.ps1 over and run it inside the VM.

To auto-start this VM on host boot: virsh autostart {VM_NAME}"""
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as e:
        # Matches `set -euo pipefail`'s abort-on-failure; the failing command's own stderr already
        # went straight to the terminal (nothing here is captured), so just stop, no traceback.
        print(f"provision-windows-vm.py: {e.args[0]} exited {e.returncode}", file=sys.stderr)
        raise SystemExit(1) from None
