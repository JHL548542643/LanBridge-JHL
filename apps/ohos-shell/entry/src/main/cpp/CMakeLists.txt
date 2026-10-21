# CMake for HarmonyOS NAPI bridge → lb_ffi.
# Prefers a prebuilt static library at ${LB_FFI_LIB} (set from DevEco / scripts).

cmake_minimum_required(VERSION 3.16)
project(lb_napi)

set(NATIVERENDER_ROOT_PATH ${CMAKE_CURRENT_SOURCE_DIR})

if(DEFINED PACKAGE_FIND_FILE)
    include(${PACKAGE_FIND_FILE})
endif()

add_library(lb_napi SHARED napi_init.cpp)

target_include_directories(lb_napi PRIVATE ${NATIVERENDER_ROOT_PATH})

# Optional: path to Rust lb_ffi.a for aarch64-unknown-linux-ohos.
# Prefer -DLB_FFI_LIB=... from DevEco; else fall back to repo target/ (relative).
if(NOT DEFINED LB_FFI_LIB OR NOT EXISTS "${LB_FFI_LIB}")
    set(_lb_fallback
        "${NATIVERENDER_ROOT_PATH}/../../../../../../target/aarch64-unknown-linux-ohos/release/liblb_ffi.a")
    if(EXISTS "${_lb_fallback}")
        set(LB_FFI_LIB "${_lb_fallback}")
    endif()
endif()

if(DEFINED LB_FFI_LIB AND EXISTS "${LB_FFI_LIB}")
    # libhilog_ndk.z.so: lb-ffi forwards tracing events to OH_LOG_Print (tag "lanbridge").
    target_link_libraries(lb_napi PUBLIC ${LB_FFI_LIB} libace_napi.z.so libhilog_ndk.z.so)
    message(STATUS "Linking lb_ffi from ${LB_FFI_LIB}")
else()
    message(WARNING "LB_FFI_LIB not set — run scripts/build-ohos-ffi.ps1 then rebuild")
    target_link_libraries(lb_napi PUBLIC libace_napi.z.so libhilog_ndk.z.so)
endif()
