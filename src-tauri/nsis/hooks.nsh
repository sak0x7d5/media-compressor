; Installer hooks for Media Compressor.
;
; The app can add a "Shrink" cascade to Explorer's right-click menu, which lives
; entirely under HKEY_CURRENT_USER\Software\Classes. Uninstalling has to take it
; with it: left behind, the shell keeps offering menu entries that launch an
; executable which is no longer there.
;
; The app's own `shell_integration::unregister` is the same operation, but it
; cannot run here — by uninstall time there may be no working copy of the app to
; run it, and a user who never opened Settings to turn the menu off would be
; left with the keys either way.
;
; This assumes a per-user install, which is Tauri's NSIS default. Setting
; bundle.windows.nsis.installMode to "perMachine" would run the uninstaller
; elevated, so HKCU would resolve to the administrator's hive and this would
; silently clean nothing.

; DeleteRegKey removes the key and everything under it. A key that was never
; written simply sets the error flag, which nothing here reads, so uninstalling
; a copy that never enabled the menu is a no-op rather than a failure.
!macro RemoveShrinkVerb EXTENSION
  DeleteRegKey HKCU "Software\Classes\SystemFileAssociations\${EXTENSION}\shell\MediaCompressorCompress"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; The one shared definition of the submenu that every extension points at.
  DeleteRegKey HKCU "Software\Classes\MediaCompressor.ShrinkMenu"

  ; Kept in step with EXTENSIONS in src-tauri/src/shell_integration.rs. A test
  ; there reads this file and fails if the two lists drift apart, because an
  ; extension added there and forgotten here is a key nothing ever removes.
  !insertmacro RemoveShrinkVerb ".mp4"
  !insertmacro RemoveShrinkVerb ".mov"
  !insertmacro RemoveShrinkVerb ".mkv"
  !insertmacro RemoveShrinkVerb ".webm"
  !insertmacro RemoveShrinkVerb ".avi"
  !insertmacro RemoveShrinkVerb ".m4v"
  !insertmacro RemoveShrinkVerb ".wmv"
  !insertmacro RemoveShrinkVerb ".flv"
  !insertmacro RemoveShrinkVerb ".mpg"
  !insertmacro RemoveShrinkVerb ".mpeg"
  !insertmacro RemoveShrinkVerb ".gif"
  !insertmacro RemoveShrinkVerb ".png"
  !insertmacro RemoveShrinkVerb ".jpg"
  !insertmacro RemoveShrinkVerb ".jpeg"
  !insertmacro RemoveShrinkVerb ".webp"
  !insertmacro RemoveShrinkVerb ".avif"
  !insertmacro RemoveShrinkVerb ".bmp"
  !insertmacro RemoveShrinkVerb ".tif"
  !insertmacro RemoveShrinkVerb ".tiff"
  ; Registered by earlier builds and retired since; the key is still there on
  ; any machine that enabled the menu under one of them. Mirrors
  ; RETIRED_EXTENSIONS in shell_integration.rs.
  !insertmacro RemoveShrinkVerb ".ts"
!macroend
