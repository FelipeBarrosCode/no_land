// Keep in sync with services/platform_credentials.rs in all three client repos.
// These values cross SSH and nested bash -lc boundaries for sunshine --creds.
// Reject unsupported input; never trim or otherwise rewrite a secret.
export function platformUsernameError(value: string): string {
  return /^[A-Za-z0-9][A-Za-z0-9._-]{2,}$/.test(value) && !/\s/.test(value)
    ? ""
    : "Username must have at least 3 characters, start with a letter or number, and use only A-Z, a-z, 0-9, dot, underscore or hyphen.";
}

export function platformPasswordError(value: string): string {
  return value.length >= 6 && !value.startsWith("-") && !/[^A-Za-z0-9._~!@#%^*+=,:?/-]/.test(value)
    ? ""
    : "Password must have at least 6 characters, must not start with a hyphen, and use only A-Z, a-z, 0-9 or . _ ~ ! @ # % ^ * + = , : ? / - (no spaces or quotes).";
}
