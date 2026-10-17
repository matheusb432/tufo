# Extends cargo-xwin's clang-cl toolchain for Opus; CEF's C++ wrapper, the other CMake build,
# needs nothing more.
include("$ENV{CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_msvc}")
set(CMAKE_PROJECT_Opus_INCLUDE "${CMAKE_CURRENT_LIST_DIR}/opus-clang-cl.cmake")
# Opus dispatches to these at run time, after checking the CPU.
set(OPUS_X86_MAY_HAVE_SSE4_1 ON CACHE BOOL "" FORCE)
set(OPUS_X86_MAY_HAVE_AVX2 ON CACHE BOOL "" FORCE)
# Opus defaults to the DLL runtime; the executables link the static one.
set(OPUS_STATIC_RUNTIME ON CACHE BOOL "" FORCE)
