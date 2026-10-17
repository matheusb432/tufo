@echo off
rem Installs the machine-wide tools the Windows build needs: the MSVC Build Tools with the
rem Windows SDK, and a pinned CMake for the bundled Opus build. Run elevated through
rem `winvm-rs script --elevated`; rerunning skips installed tools.
setlocal
set "DOWNLOADS=%SystemRoot%\Temp\tufo-provision"
if not exist "%DOWNLOADS%" mkdir "%DOWNLOADS%"

if exist "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\" goto msvc_ready
echo Installing the MSVC Build Tools and the Windows SDK...
curl.exe -fsSL -o "%DOWNLOADS%\vs_BuildTools.exe" https://aka.ms/vs/17/release/vs_BuildTools.exe || exit /b 1
"%DOWNLOADS%\vs_BuildTools.exe" --quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621
rem 3010 means the installation succeeded and a restart completes it.
if %errorlevel% equ 3010 goto msvc_ready
if %errorlevel% neq 0 exit /b %errorlevel%
:msvc_ready
echo MSVC Build Tools are installed.

if exist "%ProgramFiles%\CMake\bin\cmake.exe" goto cmake_ready
echo Installing CMake 3.31.12...
curl.exe -fsSL -o "%DOWNLOADS%\cmake.msi" https://github.com/Kitware/CMake/releases/download/v3.31.12/cmake-3.31.12-windows-x86_64.msi || exit /b 1
msiexec.exe /i "%DOWNLOADS%\cmake.msi" /qn /norestart ADD_CMAKE_TO_PATH=System
if %errorlevel% equ 3010 goto cmake_ready
if %errorlevel% neq 0 exit /b %errorlevel%
:cmake_ready
echo CMake is installed.
