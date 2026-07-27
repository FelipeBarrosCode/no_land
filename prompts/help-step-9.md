## Launch your server and follow provisioning

After selecting a server and clicking Play, Noland automatically:

1. **Creates the instance** on Vast.ai with your chosen server and storage size
2. **Waits for SSH access** (usually 1-3 minutes)
3. **Installs and configures**:
   - NVIDIA headless drivers (for streaming without a physical display)
   - Sunshine streaming server with optimized settings
   - PipeWire + WirePlumber for low-latency audio
   - WireGuard for the secure connection tunnel
4. **Sets up your local connection** — the app opens a modal guiding you through importing the WireGuard config
5. **Pairs Moonlight** — enters the PIN automatically once Sunshine is ready

The Provisioning screen shows a live timeline of each step and a scrolling log. You can watch progress in real time.

**Password**: The default password to log into your cloud machine is `password`.

Once provisioning completes, your server is ready to stream!
