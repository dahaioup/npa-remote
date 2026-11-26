# Windows OCR via WinRT (Windows.Media.Ocr).
# Invoked as: powershell -NoProfile -ExecutionPolicy Bypass -File <this> <imagePath> <lang>
# NOTE: keep this file ASCII-only; PowerShell 5.1 misreads non-BOM UTF-8 comments.
$Path = $args[0]
$Lang = $args[1]

$ErrorActionPreference = "Stop"
function Fail([string]$message) {
  @{ engine = "windows-ocr"; error = $message; lines = @() } | ConvertTo-Json -Compress -Depth 4
  exit 1
}
# PowerShell 5.1 has no GetAwaiter on WinRT IAsyncOperation; bridge it via SystemRuntime.
try { Add-Type -AssemblyName System.Runtime.WindowsRuntime } catch { Fail "System.Runtime.WindowsRuntime unavailable: $_" }
$asTaskMethods = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
  $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -like 'IAsyncOperation*'
}
if (-not $asTaskMethods) { Fail "AsTask extension not found" }
$asTaskOp = $asTaskMethods[0]
function AwaitOp($operation, [type]$resultType) {
  $asTask = $asTaskOp.MakeGenericMethod($resultType)
  $task = $asTask.Invoke($null, @($operation))
  $task.Wait()
  $task.Result
}
try {
  if (-not (Test-Path $Path)) { Fail "screenshot file missing: $Path" }
  $null = [Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType = WindowsRuntime]
  $null = [Windows.Graphics.Imaging.BitmapDecoder, Windows.Foundation, ContentType = WindowsRuntime]
  $null = [Windows.Storage.Streams.InMemoryRandomAccessStream, Windows.Foundation, ContentType = WindowsRuntime]
  $null = [Windows.Storage.Streams.DataWriter, Windows.Foundation, ContentType = WindowsRuntime]
  $engine = $null
  if ($Lang) {
    try { $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromLanguage([Windows.Globalization.Language]::new($Lang)) } catch {}
  }
  if (-not $engine) { $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages() }
  if (-not $engine) { Fail "no OCR language pack installed (settings -> time & language -> language)" }

  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $stream = New-Object Windows.Storage.Streams.InMemoryRandomAccessStream
  $writer = New-Object Windows.Storage.Streams.DataWriter($stream.GetOutputStreamAt(0))
  $writer.WriteBytes($bytes)
  AwaitOp ($writer.StoreAsync()) ([uint32]) | Out-Null
  AwaitOp ($writer.FlushAsync()) ([bool]) | Out-Null
  $decoder = AwaitOp ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
  $software = AwaitOp ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
  $result = AwaitOp ($engine.RecognizeAsync($software)) ([Windows.Media.Ocr.OcrResult])

  $lines = @()
  foreach ($line in $result.Lines) {
    $text = $line.Text
    $left = -1; $top = -1; $right = -1; $bottom = -1
    foreach ($word in $line.Words) {
      $rect = $word.BoundingRect
      if ($left -lt 0 -or $rect.X -lt $left) { $left = [int]$rect.X }
      if ($top -lt 0 -or $rect.Y -lt $top) { $top = [int]$rect.Y }
      if ([int]($rect.X + $rect.Width) -gt $right) { $right = [int]($rect.X + $rect.Width) }
      if ([int]($rect.Y + $rect.Height) -gt $bottom) { $bottom = [int]($rect.Y + $rect.Height) }
    }
    $lines += @{ text = $text; x = $left; y = $top; w = $right - $left; h = $bottom - $top }
  }
  @{ engine = "windows-ocr"; lines = $lines } | ConvertTo-Json -Compress -Depth 4
} catch {
  Fail $_.Exception.Message
}
