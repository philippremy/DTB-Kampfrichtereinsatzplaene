#!/usr/bin/env bash
# Creates the headless Windows VM (KVM/QEMU via libvirt) that hosts the
# Windows Forgejo runner — see RUNNERS.md for why Windows needs a *real*
# Windows environment (fxc, the classic Direct3D HLSL shader compiler gpui's
# Windows backend shells out to at build time, has no cross-platform build
# at all — unlike dxc, the newer DXIL/SM6+ compiler, which isn't what gpui
# wants here).
#
# Run scripts/runner-setup-linux.sh first (installs qemu-full/libvirt).
#
#   scripts/provision-windows-vm.sh
#
# What you do manually (can't be scripted — no stable, unauthenticated direct
# download link, and Microsoft's evaluation center gates it behind a EULA
# form):
#   1. Grab the Windows Server 2022 Evaluation ISO from
#      https://www.microsoft.com/evalcenter/download-windows-server-2022
#      (Windows Server, not Windows 11 — no TPM/Secure Boot fuss for a
#      headless build box, and the eval is the same 180-day deal either way)
#   2. Save it as the path this script asks for below.
# Everything else — the VirtIO driver ISO, the VM definition, starting it —
# is automatic.

set -euo pipefail

VM_NAME="dtb-ke-windows-runner"
VM_RAM_MB=16384
VM_VCPUS=6
VM_DISK_GB=120
DISK_POOL_DIR="/var/lib/libvirt/images"
WIN_ISO="$HOME/Downloads/WindowsServer2022Eval.iso"
VIRTIO_ISO="$DISK_POOL_DIR/virtio-win.iso"

have() { command -v "$1" >/dev/null 2>&1; }

for tool in virt-install virsh qemu-img; do
  have "$tool" || { echo "missing $tool — run scripts/runner-setup-linux.sh first" >&2; exit 1; }
done

if virsh dominfo "$VM_NAME" >/dev/null 2>&1; then
  echo "VM '$VM_NAME' already exists. To start over: virsh undefine $VM_NAME --remove-all-storage"
  echo "Current state:"
  virsh dominfo "$VM_NAME"
  exit 0
fi

if [[ ! -f "$WIN_ISO" ]]; then
  cat >&2 <<EOF
Windows install ISO not found at:
  $WIN_ISO

Download the Windows Server 2022 Evaluation ISO from:
  https://www.microsoft.com/evalcenter/download-windows-server-2022

...and save it to that path (or edit WIN_ISO at the top of this script),
then re-run.
EOF
  exit 1
fi

if [[ ! -f "$VIRTIO_ISO" ]]; then
  echo "fetching the VirtIO Windows driver ISO (network/disk drivers the installer needs to see the virtio disk) …"
  sudo mkdir -p "$DISK_POOL_DIR"
  sudo curl -fsSL -o "$VIRTIO_ISO" \
    "https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso"
fi

DISK="$DISK_POOL_DIR/$VM_NAME.qcow2"
if [[ ! -f "$DISK" ]]; then
  sudo qemu-img create -f qcow2 "$DISK" "${VM_DISK_GB}G"
fi

echo "creating the VM (headless — no GPU passthrough; shader *compilation* is CPU-only, we're not rendering anything) …"
sudo virt-install \
  --name "$VM_NAME" \
  --memory "$VM_RAM_MB" \
  --vcpus "$VM_VCPUS" \
  --cpu host-passthrough \
  --os-variant win2k22 \
  --disk path="$DISK",bus=virtio \
  --disk path="$WIN_ISO",device=cdrom \
  --disk path="$VIRTIO_ISO",device=cdrom \
  --network network=default,model=virtio \
  --graphics vnc,listen=0.0.0.0 \
  --video qxl \
  --boot uefi \
  --noautoconsole

cat <<EOF

VM '$VM_NAME' created and starting. Finish the Windows install over VNC:
  virsh domdisplay $VM_NAME     # or: ssh -L 5900:localhost:<port> <this box>, then any VNC client

During setup, when the installer doesn't see a disk: "Load driver" →
the VirtIO CD → viostor\\<your Windows version>\\amd64.

Once Windows is installed and you can log in:
  1. Install the VirtIO Guest Tools from the same VirtIO CD (network driver,
     QEMU guest agent — makes 'virsh shutdown' etc. work cleanly).
  2. Enable an SSH server or RDP for headless access from here on (Windows
     Server: Server Manager → Add Roles → OpenSSH Server, or
     'Add-WindowsCapability -Online -Name OpenSSH.Server').
  3. Copy scripts/runner-setup-windows.ps1 over and run it inside the VM.

To auto-start this VM on host boot: virsh autostart $VM_NAME
EOF
