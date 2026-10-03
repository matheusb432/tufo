# Opus treats clang-cl as MSVC, but Clang needs target flags on SIMD sources.
# Cross-building from Linux also needs the SDK header's exact case.
cmake_language(DEFER CALL _opus_clang_cl_fix_sources)

function(_opus_clang_cl_fix_sources)
  set(sse4_1_sources ${celt_sources_sse4_1} ${silk_sources_sse4_1} ${dnn_sources_sse4_1})
  if(sse4_1_sources)
    set_property(SOURCE ${sse4_1_sources} APPEND PROPERTY COMPILE_OPTIONS -msse4.1)
  endif()
  set(avx2_sources ${celt_sources_avx2} ${silk_sources_avx2} ${silk_sources_float_avx2} ${dnn_sources_avx2})
  if(avx2_sources)
    set_property(SOURCE ${avx2_sources} APPEND PROPERTY COMPILE_OPTIONS -mavx2 -mfma -mavx)
  endif()

  if(CMAKE_HOST_WIN32)
    return()
  endif()

  # silk/x86/NSQ_del_dec_avx2.c includes <Intsafe.h>; the SDK ships intsafe.h.
  string(REGEX MATCHALL "/imsvc [^ ]+" sdk_includes "${CMAKE_C_FLAGS}")
  list(TRANSFORM sdk_includes REPLACE "^/imsvc " "")
  find_file(OPUS_INTSAFE_HEADER intsafe.h PATHS ${sdk_includes} NO_DEFAULT_PATH REQUIRED)
  set(case_aliases "${CMAKE_BINARY_DIR}/sdk-case-aliases")
  file(MAKE_DIRECTORY "${case_aliases}")
  file(CREATE_LINK "${OPUS_INTSAFE_HEADER}" "${case_aliases}/Intsafe.h" SYMBOLIC)
  target_include_directories(opus SYSTEM PRIVATE "${case_aliases}")
endfunction()
