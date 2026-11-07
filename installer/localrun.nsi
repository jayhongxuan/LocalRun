; LocalRun 0.1.0 Windows x64 安装程序（NSIS 3.12 Unicode）
; 作者 / 版权持有人：大山
; 按用户级安装（%LOCALAPPDATA%\LocalRun），无需管理员权限；
; 自动检测并安装 WebView2 Runtime（使用内置 Evergreen Bootstrapper）；
; 写入 HKCU 卸载信息，生成开始菜单/桌面快捷方式。

Unicode true

!define APP_NAME        "LocalRun"
!define APP_VERSION     "0.1.0"
!define APP_EXE         "LocalRun.exe"
!define APP_AUTHOR      "大山"
!define APP_PUBLISHER   "大山"
!define APP_COPYRIGHT   "Copyright © 2026 大山 (Dashan). All rights reserved."
!define WEBVIEW2_KEY    "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define UNINST_KEY      "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalRun"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

Name "${APP_NAME} ${APP_VERSION}"
OutFile "dist\LocalRun_${APP_VERSION}_x64-setup.exe"
InstallDir "$LOCALAPPDATA\LocalRun"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"
RequestExecutionLevel user
ManifestDPIAware true
SetCompressor /SOLID lzma

VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey /LANG=2052 "ProductName"     "LocalRun"
VIAddVersionKey /LANG=2052 "FileDescription" "LocalRun - 本地大模型管理器"
VIAddVersionKey /LANG=2052 "FileVersion"     "${APP_VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion"  "${APP_VERSION}"
VIAddVersionKey /LANG=2052 "CompanyName"     "${APP_PUBLISHER}"
VIAddVersionKey /LANG=2052 "Author"          "${APP_AUTHOR}"
VIAddVersionKey /LANG=2052 "LegalCopyright"  "${APP_COPYRIGHT}"
VIAddVersionKey /LANG=2052 "Comments"        "作者：${APP_AUTHOR}　${APP_COPYRIGHT}"
VIAddVersionKey /LANG=1033 "ProductName"     "LocalRun"
VIAddVersionKey /LANG=1033 "FileDescription" "LocalRun - Local LLM Manager"
VIAddVersionKey /LANG=1033 "FileVersion"     "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion"  "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "CompanyName"     "${APP_PUBLISHER}"
VIAddVersionKey /LANG=1033 "LegalCopyright"  "${APP_COPYRIGHT}"

!define MUI_ICON "payload\icon.ico"
!define MUI_UNICON "payload\icon.ico"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "安装 LocalRun ${APP_VERSION}"
!define MUI_WELCOMEPAGE_TEXT "将在这台电脑上安装 LocalRun ${APP_VERSION}（本地大模型管理器）。$\r$\n$\r$\n作者：${APP_AUTHOR}$\r$\n${APP_COPYRIGHT}$\r$\n$\r$\n安装到当前用户目录，无需管理员权限。点击“下一步”继续。"

; ---------- 安装向导页面 ----------
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "$(RunAppAfterInstall)"
!insertmacro MUI_PAGE_FINISH

; ---------- 卸载向导页面 ----------
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; ---------- 语言（简体中文优先，自动按系统语言选择） ----------
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

LangString RunAppAfterInstall ${LANG_SIMPCHINESE} "安装完成后运行 LocalRun"
LangString RunAppAfterInstall ${LANG_ENGLISH}    "Run LocalRun after installation"
LangString SecMain       ${LANG_SIMPCHINESE} "LocalRun 主程序（必需）"
LangString SecMain       ${LANG_ENGLISH}     "LocalRun application (required)"
LangString SecDesktop    ${LANG_SIMPCHINESE} "创建桌面快捷方式"
LangString SecDesktop    ${LANG_ENGLISH}     "Create desktop shortcut"
LangString WebView2Needed ${LANG_SIMPCHINESE} "未检测到 Microsoft WebView2 运行时（Tauri 渲染需要）。$\n点击“确定”使用内置安装器联网安装（需保持网络连接），点击“取消”退出。"
LangString WebView2Needed ${LANG_ENGLISH}    "Microsoft WebView2 Runtime was not detected (required by Tauri).$\nClick OK to install it with the bundled bootstrapper (network required), or Cancel to abort."

Function .onInit
  ; MUI 依据系统语言自动选择（首个语言为 SimpChinese）
FunctionEnd

; ---------- WebView2 检测与安装 ----------
!macro EnsureWebView2
  ClearErrors
  ReadRegStr $R0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_KEY}" "pv"
  ${If} $R0 == ""
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_KEY}" "pv"
  ${EndIf}
  ${If} $R0 == ""
    ReadRegStr $R0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_KEY}" "pv"
  ${EndIf}
  ${If} $R0 == ""
    MessageBox MB_OKCANCEL|MB_ICONQUESTION "$(WebView2Needed)" IDOK +2
      Abort
    InitPluginsDir
    File "/oname=$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" "payload\MicrosoftEdgeWebview2Setup.exe"
    nsExec::ExecToLog '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" /silent /install'
    Pop $R1
  ${EndIf}
!macroend

; ---------- 安装区段 ----------
Section "!$(SecMain)" SEC_MAIN
  SectionIn RO

  ; 关闭正在运行的实例
  nsExec::Exec 'taskkill /IM "${APP_EXE}" /F'
  Pop $R0
  Sleep 500

  !insertmacro EnsureWebView2

  SetOutPath "$INSTDIR"
  File "/oname=${APP_EXE}" "payload\LocalRun.exe"
  File "/oname=localrun-icon.ico" "payload\icon.ico"

  ; 卸载信息（HKCU，用户级）
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayName"     "${APP_NAME}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayVersion"  "${APP_VERSION}"
  WriteRegStr   HKCU "${UNINST_KEY}" "Publisher"       "${APP_PUBLISHER}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayIcon"     "$INSTDIR\${APP_EXE}"
  WriteRegStr   HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr   HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr   HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  WriteRegStr   HKCU "Software\LocalRun" "" "$INSTDIR"

  ; 开始菜单快捷方式
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut  "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortcut  "$SMPROGRAMS\${APP_NAME}\卸载 LocalRun.lnk" "$INSTDIR\uninstall.exe"

  ; 卸载器
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Section "$(SecDesktop)" SEC_DESKTOP
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
SectionEnd

; 默认勾选桌面快捷方式
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_MAIN}    "$(SecMain)"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DESKTOP} "$(SecDesktop)"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ---------- 卸载区段 ----------
Section "Uninstall"
  nsExec::Exec 'taskkill /IM "${APP_EXE}" /F'
  Pop $R0
  Sleep 500

  Delete "$DESKTOP\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\卸载 LocalRun.lnk"
  RMDir  "$SMPROGRAMS\${APP_NAME}"

  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\localrun-icon.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\LocalRun"
SectionEnd
