/** Static program: request data, including credentials, is read exclusively from stdin. */
export const BITLOCKER_COMMAND = String.raw`
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
$namespace = 'Root\CIMV2\Security\MicrosoftVolumeEncryption'
$session = $null
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$globalErrors = [Collections.Generic.List[object]]::new()
$operation = $null
function Failure($source, $code, $message) {
  return @{ source = $source; code = $code; message = $message }
}
function Exception-Code($record) {
  $match = [regex]::Match([string]$record.FullyQualifiedErrorId, '0x([0-9a-fA-F]{8})')
  if ($match.Success) { return [Convert]::ToUInt32($match.Groups[1].Value, 16) }
  return [uint32]([int64]$record.Exception.HResult -band [int64]4294967295)
}
function Exception-Message($record) {
  return [regex]::Replace([string]$record.Exception.Message, '\d{6}(?:-\d{6}){7}|(?<!\d)\d{48}(?!\d)', '[BitLocker 恢复密码已隐藏]')
}
function Read-Method($volume, $method, $arguments, $errors) {
  try {
    $result = Invoke-CimMethod -CimSession $session -InputObject $volume -MethodName $method -Arguments $arguments
    if ($null -eq $result.ReturnValue) { throw 'Missing return code' }
    if ($result.ReturnValue -ne 0) {
      $errors.Add((Failure $method ([uint32]$result.ReturnValue) 'Windows 方法返回失败'))
      return $null
    }
    return $result
  } catch {
    $errors.Add((Failure $method (Exception-Code $_) (Exception-Message $_)))
    return $null
  }
}
function Read-Field($result, $field, $method, $errors) {
  if ($null -eq $result) { return $null }
  if ($null -eq $result.$field) {
    $errors.Add((Failure $method $null ('Windows 方法缺少字段：' + $field)))
  }
  return $result.$field
}
function Read-Volume($volume) {
  $errors = [Collections.Generic.List[object]]::new()
  $conversion = Read-Method $volume 'GetConversionStatus' @{ PrecisionFactor = [uint32]0 } $errors
  $algorithm = Read-Method $volume 'GetEncryptionMethod' @{} $errors
  $protection = Read-Method $volume 'GetProtectionStatus' @{} $errors
  $lock = Read-Method $volume 'GetLockStatus' @{} $errors
  $autoEnabled = $null
  $autoStored = $null
  if ($null -ne $volume.VolumeType) {
    if ($volume.VolumeType -eq 0) {
      $auto = Read-Method $volume 'IsAutoUnlockKeyStored' @{} $errors
      $autoStored = Read-Field $auto 'IsAutoUnlockKeyStored' 'IsAutoUnlockKeyStored' $errors
    } else {
      $auto = Read-Method $volume 'IsAutoUnlockEnabled' @{} $errors
      $autoEnabled = Read-Field $auto 'IsAutoUnlockEnabled' 'IsAutoUnlockEnabled' $errors
    }
  }
  $protectors = [Collections.Generic.List[object]]::new()
  $keys = Read-Method $volume 'GetKeyProtectors' @{ KeyProtectorType = [uint32]0 } $errors
  if ($null -ne $keys) {
    foreach ($id in $keys.VolumeKeyProtectorID) {
      $kind = Read-Method $volume 'GetKeyProtectorType' @{ VolumeKeyProtectorID = [string]$id } $errors
      $protectors.Add(@{ id = [string]$id; type = (Read-Field $kind 'KeyProtectorType' 'GetKeyProtectorType' $errors) })
    }
  }
  return @{
    volumeId = [string]$volume.DeviceID
    drive = $(if ($volume.DriveLetter) { [string]$volume.DriveLetter } else { $null })
    volumeType = $volume.VolumeType
    conversionStatus = (Read-Field $conversion 'ConversionStatus' 'GetConversionStatus' $errors)
    encryptionPercentage = (Read-Field $conversion 'EncryptionPercentage' 'GetConversionStatus' $errors)
    encryptionMethod = (Read-Field $algorithm 'EncryptionMethod' 'GetEncryptionMethod' $errors)
    protectionStatus = (Read-Field $protection 'ProtectionStatus' 'GetProtectionStatus' $errors)
    lockStatus = (Read-Field $lock 'LockStatus' 'GetLockStatus' $errors)
    autoUnlockEnabled = $autoEnabled
    autoUnlockKeysStored = $autoStored
    keyProtectors = @($protectors.ToArray())
    collectionErrors = @($errors.ToArray())
  }
}
$volumes = @()
try {
  $option = New-CimSessionOption -Protocol Dcom
  if ($option.PacketPrivacy -ne $true) { throw 'BitLocker CIM 连接要求 PacketPrivacy' }
  $session = New-CimSession -ComputerName localhost -SessionOption $option
  $instances = @(Get-CimInstance -CimSession $session -Namespace $namespace -ClassName Win32_EncryptableVolume)
  if ($request.volumeId) {
    $instances = @($instances | Where-Object { $_.DeviceID -eq $request.volumeId })
  } elseif ($request.drive) {
    $instances = @($instances | Where-Object { $_.DriveLetter -eq $request.drive })
  }
  if ($request.scope -eq 'status') {
    $volumes = @($instances | ForEach-Object { Read-Volume $_ })
  } else {
    if ($instances.Count -ne 1 -or -not $request.volumeId) { throw 'Target unavailable' }
    $volume = $instances[0]
    $before = Read-Volume $volume
    $operation = @{ status = 'rejected'; reason = '无法满足执行条件'; returnCode = $null; error = $null }
    if (-not $admin) {
      $operation.reason = '需要管理员权限'
    } elseif ($request.drive -and $volume.DriveLetter -ne $request.drive) {
      $operation.reason = '目标卷盘符已变化'
    } elseif ($request.scope -eq 'unlock' -and $before.lockStatus -eq 0) {
      $operation.status = 'skipped'; $operation.reason = '卷已解锁'
    } elseif ($request.scope -eq 'decrypt' -and $before.conversionStatus -eq 0) {
      $operation.status = 'skipped'; $operation.reason = '卷已完全解密'
    } elseif ($request.scope -eq 'decrypt' -and $before.conversionStatus -eq 3) {
      $operation.status = 'skipped'; $operation.reason = '卷正在解密'
    } elseif ($null -eq $before.lockStatus -or ($request.scope -eq 'decrypt' -and $null -eq $before.conversionStatus)) {
      $operation.reason = '执行所需状态采集失败'
    } elseif ($request.scope -eq 'decrypt' -and $before.lockStatus -ne 0) {
      $operation.reason = '卷已锁定'
    } elseif ($request.scope -eq 'decrypt' -and $before.conversionStatus -notin @(1,2,4,5)) {
      $operation.reason = '转换状态不支持执行'
    } elseif ($request.scope -eq 'unlock' -or $request.scope -eq 'decrypt') {
      $method = $(if ($request.scope -eq 'unlock') { 'UnlockWithNumericalPassword' } else { 'Decrypt' })
      $arguments = $(if ($request.scope -eq 'unlock') { @{ NumericalPassword = [string]$request.recoveryPassword } } else { @{} })
      $operation.status = 'unknown'; $operation.reason = '未收到 Windows 操作返回'
      try {
        $result = Invoke-CimMethod -CimSession $session -InputObject $volume -MethodName $method -Arguments $arguments
        if ($null -ne $result.ReturnValue) {
          $operation.returnCode = [uint32]$result.ReturnValue
          if ($result.ReturnValue -eq 0) {
            $operation.status = 'accepted'; $operation.reason = 'Windows 已接受请求'
          } else {
            $operation.status = 'rejected'; $operation.reason = 'Windows 拒绝请求'
            $operation.error = Failure $method ([uint32]$result.ReturnValue) 'Windows 操作返回失败'
          }
        }
      } catch {
        $operation.error = Failure $method (Exception-Code $_) (Exception-Message $_)
      } finally {
        $arguments = $null
        if ($request.PSObject.Properties['recoveryPassword']) { $request.recoveryPassword = $null }
      }
    }
    $volumes = @((Read-Volume $volume))
  }
} catch {
  $globalErrors.Add((Failure 'Win32_EncryptableVolume' (Exception-Code $_) (Exception-Message $_)))
} finally {
  if ($session) { Remove-CimSession $session -ErrorAction SilentlyContinue }
}
$output = @{ snapshot = @{ collectedAt = [DateTime]::UtcNow.ToString('o'); admin = $admin; volumes = @($volumes); collectionErrors = @($globalErrors.ToArray()) } }
if ($null -ne $operation) { $output.operation = $operation }
$output | ConvertTo-Json -Depth 12 -Compress
`;
