# Environment report

`collect_environment.ps1` saves a JSON report containing Windows version, GPU and network-driver versions, network link/error counters, Rubylight version/hash and service state. It reads the local system and runs Rubylight's `--version` command. It does not start a stream, change settings or upload the report.

For an installed copy, open PowerShell in a writable folder and run:

```powershell
powershell.exe -NoProfile -File "C:\Program Files\Butterpollo\tools\collect_environment.ps1"
```

For a portable copy, run from its extracted folder:

```powershell
powershell.exe -NoProfile -File ".\tools\collect_environment.ps1" -InstallDirectory "."
```

The timestamped report is saved in the current folder. Use `-OutputFile` to choose another new filename; existing reports are never overwritten. Review the JSON before sharing it. Error messages can contain local system details. The report helps reproduce hardware-specific issues; it is not a latency measurement or proof of Wi-Fi quality.
