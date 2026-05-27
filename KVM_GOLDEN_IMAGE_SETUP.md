# KVM Golden Image Setup (Ubuntu 26.04 Target)

## 1. Overview

This guide defines a practical local virtualization workflow using KVM/libvirt + virt-manager, centered on an Ubuntu 26.04 golden image.

Architecture:

```text
Host OS
 ├─ KVM kernel virtualization
 ├─ QEMU/libvirt
 ├─ virt-manager GUI
 ├─ Ubuntu 26.04 golden qcow2 template image
 └─ cloned/overlay VM instances
```

Disk model definitions:

- **Normal VM disk**: standard standalone disk for a single VM.
- **Template/golden image**: sealed base disk that you reuse for future VMs.
- **Full clone**: full copy of template disk; easiest, safest, more storage.
- **qcow2 linked clone/overlay**: child disk references template backing file; fast and space-efficient.

Important rule:

- Once a VM disk becomes the golden template, do not boot it directly for normal use.

End goal:

```text
Install/configure Ubuntu 26.04 once → seal it → clone/reuse it quickly
```

---

## 2. Host preparation

Install the KVM stack:

```bash
sudo apt update
sudo apt install -y \
  qemu-kvm \
  libvirt-daemon-system \
  libvirt-clients \
  virt-manager \
  virtinst \
  qemu-utils \
  bridge-utils \
  ovmf \
  swtpm \
  cloud-image-utils \
  osinfo-db \
  libosinfo-bin
```

Enable libvirt:

```bash
sudo systemctl enable --now libvirtd
```

Grant user access:

```bash
sudo usermod -aG libvirt,kvm $USER
```

Log out/in (or reboot) so new group membership applies.

Verify stack:

```bash
virsh list --all
lsmod | grep kvm
```

Optional CPU acceleration check:

```bash
sudo apt install -y cpu-checker
kvm-ok
```

---

## 3. Ubuntu 26.04 osinfo compatibility check

Check whether local `osinfo-db` recognizes Ubuntu 26.04:

```bash
osinfo-query os | grep -i ubuntu
```

Use this logic:

- If `ubuntu26.04` exists, you may use:

```bash
--os-variant ubuntu26.04
```

- If it does not exist, use:

```bash
--os-variant detect=on,require=off
```

- Fallback only (if needed):

```bash
--os-variant ubuntu24.04
```

Warning:

```text
Do not block the setup just because the exact Ubuntu 26.04 osinfo entry is missing.
Use detect=on,require=off when needed.
```

Preference for this document:

- Use `--os-variant detect=on,require=off` in examples unless explicitly noted that `ubuntu26.04` is available.

---

## 4. BIOS/UEFI settings

Enable in firmware:

- Intel VT-x or AMD-V
- Intel VT-d or AMD-Vi/IOMMU
- Above 4G Decoding (recommended for passthrough)
- Resizable BAR (optional)
- SR-IOV (optional)

Set Linux kernel parameters:

- Intel:

```text
intel_iommu=on iommu=pt
```

- AMD:

```text
amd_iommu=on iommu=pt
```

Edit GRUB:

```bash
sudo nano /etc/default/grub
sudo update-grub
sudo reboot
```

Verify after reboot:

```bash
dmesg | grep -e DMAR -e IOMMU
```

---

## 5. Recommended host folder structure

Create layout:

```bash
sudo mkdir -p /var/lib/libvirt/images/templates
sudo mkdir -p /var/lib/libvirt/images/instances
sudo mkdir -p /var/lib/libvirt/iso
```

Intended structure:

```text
/var/lib/libvirt/images/
├── templates/
│   └── ubuntu-26-04-gaming-base.qcow2
├── instances/
│   ├── gaming-test.qcow2
│   └── dev-test.qcow2
└── iso/
    └── ubuntu-26.04.iso
```

Note: file ownership/permissions may need to match `libvirt`/`qemu` users on your distro.

---

## 6. Getting Ubuntu 26.04 install media

Download Ubuntu 26.04 ISO from the official Ubuntu releases source.

Example filenames:

```text
ubuntu-26.04-desktop-amd64.iso
ubuntu-26.04-live-server-amd64.iso
```

Copy ISO into libvirt storage:

```bash
sudo cp ~/Downloads/ubuntu-26.04-desktop-amd64.iso /var/lib/libvirt/iso/
```

or:

```bash
sudo cp ~/Downloads/ubuntu-26.04-live-server-amd64.iso /var/lib/libvirt/iso/
```

Use case guidance:

- **Desktop ISO**: better for GUI, Sunshine/Moonlight, Steam, gaming experiments.
- **Server ISO**: lighter for dev/server roles.
- For cloud-gaming style experiments, start with Desktop.

---

## 7. Creating the first Ubuntu 26.04 VM with virt-manager

In virt-manager:

```text
File → New Virtual Machine
Local install media
Choose Ubuntu 26.04 ISO
Allocate CPU/RAM/disk
Customize before install
```

Recommended settings:

- Firmware: UEFI/OVMF
- CPU model: host-passthrough (if available)
- Disk bus: VirtIO
- Network model: VirtIO
- Display: Spice
- Video: VirtIO or QXL
- Add qemu guest agent channel
- Disk format: qcow2

Initial sizing example:

```text
CPU: 4–8 vCPUs
RAM: 8–16 GB
Disk: 80–150 GB qcow2
```

Note: Windows guests need VirtIO drivers, but this guide targets Ubuntu 26.04 guests.

---

## 8. Creating the first Ubuntu 26.04 VM with virt-install

```bash
sudo virt-install \
  --name ubuntu-26-04-base \
  --memory 16384 \
  --vcpus 8 \
  --cpu host-passthrough \
  --disk path=/var/lib/libvirt/images/ubuntu-26-04-base.qcow2,size=120,format=qcow2,bus=virtio \
  --cdrom /var/lib/libvirt/iso/ubuntu-26.04-desktop-amd64.iso \
  --network network=default,model=virtio \
  --graphics spice \
  --video virtio \
  --boot uefi \
  --os-variant detect=on,require=off
```

If `ubuntu26.04` is recognized by `osinfo-query`, you may use:

```bash
--os-variant ubuntu26.04
```

---

## 9. Ubuntu 26.04 guest base configuration

Inside the guest:

```bash
sudo apt update
sudo apt install -y \
  curl \
  wget \
  git \
  unzip \
  vim \
  nano \
  htop \
  btop \
  tmux \
  openssh-server \
  qemu-guest-agent \
  spice-vdagent \
  wireguard \
  pipewire \
  pipewire-pulse \
  wireplumber \
  xserver-xorg \
  dbus-x11 \
  pciutils \
  usbutils \
  mesa-utils \
  vulkan-tools \
  ffmpeg
```

Enable services:

```bash
sudo systemctl enable --now ssh
sudo systemctl enable --now qemu-guest-agent
```

Create normal user:

```bash
sudo adduser gamer
sudo usermod -aG sudo,video,audio,input gamer
```

Optional passwordless sudo:

```bash
echo "gamer ALL=(ALL) NOPASSWD:ALL" | sudo tee /etc/sudoers.d/gamer
sudo chmod 440 /etc/sudoers.d/gamer
```

If package names differ in Ubuntu 26.04:

```bash
apt search <package-name>
```

---

## 10. Optional gaming/remote desktop packages

Optional stack:

- Sunshine
- WireGuard
- NVIDIA drivers (if passthrough)
- PipeWire audio
- Steam / Heroic / Lutris / Bottles

Sunshine example (verify package naming/version first):

```bash
mkdir -p ~/Downloads
cd ~/Downloads

# Verify the latest package URL before using this.
# Use the Ubuntu 26.04 package if available.
# If no 26.04 package exists, test the closest supported Ubuntu package.
wget https://github.com/LizardByte/Sunshine/releases/latest/download/sunshine-ubuntu-24.04-amd64.deb
sudo apt install -y ./sunshine-ubuntu-24.04-amd64.deb
```

Enable linger for non-root runtime services:

```bash
sudo loginctl enable-linger gamer
```

Notes:

```text
Sunshine may need display/EDID/NVIDIA-specific configuration depending on VM and passthrough setup.
```

For cloud-gaming style guests you usually want:

- Desktop environment
- Working GPU acceleration
- PipeWire audio
- Sunshine
- WireGuard
- Stable virtual/physical display path

---

## 11. Optional NVIDIA GPU passthrough notes

Host checks:

```bash
lspci -nn | grep -i nvidia
lspci -nnk -d 10de:
```

Concept summary:

- Bind GPU + GPU audio functions to `vfio-pci`.
- Blacklist host NVIDIA driver if host should not own that GPU.
- Attach PCI devices in virt-manager to guest.
- Install NVIDIA driver inside Ubuntu 26.04 guest.
- Verify with `nvidia-smi`.

Guest verification:

```bash
nvidia-smi
ffmpeg -encoders | grep nvenc
```

Limitation:

```text
The simplest reliable model is one physical GPU passed through to one active VM.
Sharing one consumer GPU across many VMs is not the goal here.
```

For a single personal host, baking NVIDIA drivers into the Ubuntu 26.04 golden image is reasonable if hardware is stable.

---

## 12. Optional WireGuard reference setup

Install:

```bash
sudo apt install -y wireguard
```

Config path:

```text
/etc/wireguard/wg0.conf
```

Enable service:

```bash
sudo systemctl enable wg-quick@wg0
```

Start only after config exists:

```bash
sudo systemctl start wg-quick@wg0
```

Helper script:

```bash
sudo nano /usr/local/bin/setup-wireguard.sh
```

Script content:

```bash
#!/usr/bin/env bash
set -euo pipefail

WG_CONF="${1:-/tmp/wg0.conf}"

if [ ! -f "$WG_CONF" ]; then
  echo "Usage: setup-wireguard.sh /path/to/wg0.conf"
  exit 1
fi

sudo cp "$WG_CONF" /etc/wireguard/wg0.conf
sudo chmod 600 /etc/wireguard/wg0.conf
sudo systemctl enable --now wg-quick@wg0
```

Make executable:

```bash
sudo chmod +x /usr/local/bin/setup-wireguard.sh
```

Template warning:

```text
Do not bake private WireGuard keys into a reusable template unless this is strictly single-user/single-host and you accept the security tradeoff.
```

---

## 13. Cleaning and sealing the Ubuntu 26.04 template

Before sealing the template, remove unique machine state.

Inside guest:

```bash
sudo apt clean
sudo rm -rf /tmp/* /var/tmp/*
history -c
```

Reset machine-id:

```bash
sudo truncate -s 0 /etc/machine-id
sudo rm -f /var/lib/dbus/machine-id
sudo ln -s /etc/machine-id /var/lib/dbus/machine-id
```

Remove SSH host keys:

```bash
sudo rm -f /etc/ssh/ssh_host_*
```

If cloud-init present:

```bash
sudo cloud-init clean --logs
```

Shutdown:

```bash
sudo shutdown now
```

Why:

- Avoid duplicate machine-id across clones.
- Avoid duplicate SSH host keys.
- Remove stale temp/cache/log state.
- Ensure template is clean and reusable.

---

## 14. First-boot reset script for clones

Create script:

```bash
sudo nano /usr/local/bin/firstboot-template-reset.sh
```

Content:

```bash
#!/usr/bin/env bash
set -euo pipefail

MARKER="/var/lib/firstboot-template-reset.done"

if [ -f "$MARKER" ]; then
  exit 0
fi

if [ ! -s /etc/machine-id ]; then
  systemd-machine-id-setup
fi

if ! ls /etc/ssh/ssh_host_* >/dev/null 2>&1; then
  ssh-keygen -A
fi

CURRENT_HOSTNAME="$(hostname)"
if [[ "$CURRENT_HOSTNAME" == "template"* || "$CURRENT_HOSTNAME" == "ubuntu-26-04-base" || "$CURRENT_HOSTNAME" == "ubuntu-26-04-gaming-base" ]]; then
  NEW_HOSTNAME="vm-$(openssl rand -hex 3)"
  hostnamectl set-hostname "$NEW_HOSTNAME"
fi

touch "$MARKER"
```

Make executable:

```bash
sudo chmod +x /usr/local/bin/firstboot-template-reset.sh
```

Create service:

```bash
sudo nano /etc/systemd/system/firstboot-template-reset.service
```

Content:

```ini
[Unit]
Description=First boot reset for cloned VM templates
After=network.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/firstboot-template-reset.sh

[Install]
WantedBy=multi-user.target
```

Enable service:

```bash
sudo systemctl enable firstboot-template-reset.service
```

Before final shutdown of template:

```bash
sudo rm -f /var/lib/firstboot-template-reset.done
sudo truncate -s 0 /etc/machine-id
sudo rm -f /etc/ssh/ssh_host_*
sudo shutdown now
```

---

## 15. Moving the template disk

Move sealed base disk:

```bash
sudo mv /var/lib/libvirt/images/ubuntu-26-04-base.qcow2 \
  /var/lib/libvirt/images/templates/ubuntu-26-04-gaming-base.qcow2
```

Rule:

- Do not boot this base disk directly for regular usage after it becomes the template.

Optional read-only protection:

```bash
sudo chmod 444 /var/lib/libvirt/images/templates/ubuntu-26-04-gaming-base.qcow2
```

Warning:

```text
If you change the base image while overlays depend on it, you can break or corrupt overlay VMs.
```

If you must update the base, make writable temporarily, perform controlled maintenance, reseal, then lock again.

---

## 16. Creating a full clone from virt-manager

GUI flow:

```text
Right click VM → Clone
Choose new name
Clone disk
Start new VM
```

Notes:

- Easiest approach.
- Uses more disk space.
- Good starter workflow.

---

## 17. Creating a qcow2 overlay clone

Create overlay:

```bash
sudo qemu-img create -f qcow2 \
  -F qcow2 \
  -b /var/lib/libvirt/images/templates/ubuntu-26-04-gaming-base.qcow2 \
  /var/lib/libvirt/images/instances/gaming-test.qcow2
```

Verify:

```bash
qemu-img info /var/lib/libvirt/images/instances/gaming-test.qcow2
```

Create VM from overlay:

```bash
sudo virt-install \
  --name gaming-test \
  --memory 16384 \
  --vcpus 8 \
  --cpu host-passthrough \
  --disk path=/var/lib/libvirt/images/instances/gaming-test.qcow2,format=qcow2,bus=virtio \
  --network network=default,model=virtio \
  --graphics spice \
  --video virtio \
  --boot uefi \
  --os-variant detect=on,require=off \
  --import
```

If available in osinfo-db, you may use:

```bash
--os-variant ubuntu26.04
```

Alternative: create/import the overlay disk through virt-manager GUI.

---

## 18. Network modes

Default libvirt NAT network:

```text
Host has normal network
VM gets 192.168.122.x
VM can access internet
External machines cannot directly access VM unless ports are forwarded
```

Bridge mode:

```text
VM appears as another machine on LAN
Better for direct local access
Requires host bridge config
```

For Moonlight/WireGuard style setups:

```text
Client → WireGuard → VM private IP → Sunshine
```

Common Sunshine/Moonlight ports reference:

```text
TCP 47990 - Sunshine web UI
TCP 47984/47989 - control
UDP 47998 - video
UDP 47999 - control
UDP 48000 - audio
```

Guidance:

```text
If using WireGuard, often you only need to expose WireGuard UDP publicly, not every Sunshine port.
```

---

## 19. Snapshot and rollback workflow

Virt-manager snapshots:

```text
Open VM → Snapshots → Create Snapshot
```

CLI examples:

```bash
virsh snapshot-create-as --domain gaming-test --name clean-install
virsh snapshot-list gaming-test
```

Notes:

- Snapshots are useful before risky changes.
- Overlays are better for disposable clean-session patterns.
- Snapshots do not replace a real golden image strategy.

---

## 20. Updating the golden image

Safe workflow:

```text
1. Stop all overlay VMs that depend on the base.
2. Make a backup of the base qcow2.
3. Temporarily make the base writable.
4. Boot a maintenance VM using the base.
5. Apply updates.
6. Clean and reseal.
7. Shut down.
8. Mark base read-only again.
9. Recreate new overlays from the updated base.
```

Warning:

```text
Changing a backing file while overlays depend on it can corrupt or break those overlays.
```

Backup example:

```bash
sudo cp /var/lib/libvirt/images/templates/ubuntu-26-04-gaming-base.qcow2 \
  /var/lib/libvirt/images/templates/ubuntu-26-04-gaming-base.backup.qcow2
```

---

## 21. Troubleshooting

KVM unavailable:

```bash
lsmod | grep kvm
dmesg | grep -i kvm
```

User cannot access libvirt:

```bash
groups
sudo usermod -aG libvirt,kvm $USER
```

VM disk permission issues:

```bash
sudo chown -R libvirt-qemu:kvm /var/lib/libvirt/images
```

Guest agent not responding:

```bash
sudo systemctl status qemu-guest-agent
```

No internet in VM:

```bash
virsh net-list --all
virsh net-start default
virsh net-autostart default
```

Ubuntu 26.04 not recognized by virt-install:

```bash
osinfo-query os | grep -i ubuntu
```

Then use:

```bash
--os-variant detect=on,require=off
```

Performance issues:

- Ensure VirtIO disk/network.
- Use host-passthrough CPU.
- Avoid heavy desktop effects.
- Allocate enough RAM/vCPU.
- Prefer SSD/NVMe storage.
- Confirm KVM acceleration is active.

GPU passthrough issues:

- Verify IOMMU enabled.
- Verify GPU isolated in IOMMU group.
- Verify host is not binding target GPU.
- Verify `vfio-pci` binding.
- Verify guest sees GPU (`lspci`).
- Verify guest driver (`nvidia-smi`).

---

## 22. Final checklist

```text
Host:
[ ] KVM installed
[ ] libvirt running
[ ] user in libvirt/kvm groups
[ ] virt-manager opens
[ ] default network active
[ ] osinfo checked for Ubuntu 26.04 support

Ubuntu 26.04 template VM:
[ ] OS installed
[ ] qemu-guest-agent installed
[ ] SSH enabled
[ ] baseline packages installed
[ ] optional Sunshine/WireGuard installed
[ ] optional NVIDIA driver installed if using GPU passthrough
[ ] firstboot reset service installed
[ ] machine-id cleared
[ ] SSH host keys removed
[ ] VM shut down cleanly

Golden image:
[ ] disk moved to templates folder
[ ] base marked read-only
[ ] overlay clone created
[ ] overlay VM boots
[ ] clone gets unique hostname/machine-id
[ ] clone has network
[ ] clone can be accessed by SSH or GUI
```

---

## Ubuntu 24.04 fallback policy (explicit)

Ubuntu 26.04 is the primary target for guest and references in this document.

Use Ubuntu 24.04 LTS only as a temporary fallback when:

- tooling does not yet recognize Ubuntu 26.04 in osinfo,
- a package/repo build for 26.04 does not exist yet,
- or install media availability is delayed.

When using fallback, keep the same workflow and migrate back to Ubuntu 26.04 as soon as practical.
