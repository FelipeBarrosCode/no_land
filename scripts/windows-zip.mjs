// Windows PowerShell 5.1's Archive module can call an unavailable three-argument
// .NET overload on ARM64 runners. Both Windows PowerShell and PowerShell 7 support
// this two-argument API. Callers must remove stale extraction directories first;
// never rely on overwrite support or fall back to an unverified archive.
export function windowsZipExtractionScript(archive, destination) {
  const quote = value => `'${String(value).replaceAll("'", "''")}'`;
  return [
    "$ErrorActionPreference = 'Stop'",
    'Add-Type -AssemblyName System.IO.Compression.FileSystem',
    `[System.IO.Compression.ZipFile]::ExtractToDirectory(${quote(archive)}, ${quote(destination)})`,
  ].join('; ');
}
