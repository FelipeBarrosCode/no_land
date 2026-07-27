You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up SFTP Storage

1. Ensure your SFTP server is running and accessible from your Noland instances
2. Collect the following information:
   - **Host** — your server's hostname or IP (e.g., `sftp.example.com`)
   - **Port** — usually `22`
   - **Username** — your SFTP user
   - **Authentication** — choose one:
     - **Password** — your SFTP user's password
     - **SSH Key** — generate an SSH key pair and add the public key to `~/.ssh/authorized_keys` on the server
   - **Remote Path** — an absolute path like `/home/user/noland-backups`
3. Optionally, **pin the host key** to prevent MITM attacks:
   - Run `ssh-keyscan -t ed25519 sftp.example.com` to get the SHA256 fingerprint
4. In Noland, paste all fields and test the connection

✅ Works with any standard SFTP/SSH server. For large backups, ensure your server has sufficient disk space and bandwidth. Recommend at least 100 GB free space per application bundle.
