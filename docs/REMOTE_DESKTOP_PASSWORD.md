# Remote desktop password

New/incomplete provisioning applies the saved NoLand platform password to the
configured Linux desktop account (`user` by default), before agent/display setup.
The username, home directory, administrative SSH key access, and SSH server
configuration are unchanged. Password-based SSH is not enabled.

Only a `desktopPasswordConfigured` completion flag is added to machine state;
the password remains in the existing protected platform credentials. Retrying
completed password setup never silently rotates the password. Older machines
that already have Sunshine configured or completed provisioning are left alone.

In Settings > Profile, save the platform password first, then use **Apply saved
password to machine #...** for each existing running machine. Saving credentials
alone does not update existing machines or their Sunshine credentials. Each
explicit action reports its own success/failure and is blocked during provisioning
or conflicting lifecycle work.

The update requires root SSH or passwordless sudo. Root/UID-zero target accounts
are rejected. A single validated `username:password` record is sent through SSH
stdin, not command arguments or environment variables. No plaintext password is
written to a remote temporary file. PAM output is suppressed and failures are
reported using sanitized categories. Success requires `chpasswd` success and an
unlocked password status; this is not an end-to-end desktop login test. A timeout
or failed post-write verification may occur after the password has changed; retry
the same saved password rather than assuming the old password was retained.

`node --test scripts/desktop-password.test.mjs` runs the actual shell script with
mock Linux account tools and checks native/UI wiring without touching any real
account. Rust service unit tests cover unsafe targets and command construction.
