cmake_minimum_required(VERSION 3.22.1)

project(auraw_android_native_deps LANGUAGES C CXX)

include(ExternalProject)
include(FetchContent)
include(ProcessorCount)

if(NOT ANDROID)
    message(FATAL_ERROR "AuRaw native dependencies must be configured with the Android NDK toolchain")
endif()

set(AURAW_SUPPORTED_ABIS arm64-v8a)
if(NOT ANDROID_ABI IN_LIST AURAW_SUPPORTED_ABIS)
    message(FATAL_ERROR
        "Unsupported AuRaw ABI '${ANDROID_ABI}'. Supported ABIs: arm64-v8a")
endif()

if(NOT DEFINED AURAW_NATIVE_STAGE_ROOT)
    get_filename_component(
        AURAW_REPOSITORY_ROOT
        "${CMAKE_CURRENT_LIST_DIR}/../../../../.."
        ABSOLUTE
    )
    set(AURAW_NATIVE_STAGE_ROOT "${AURAW_REPOSITORY_ROOT}/android/native")
endif()

if(NOT DEFINED AURAW_ANDROID_MIN_SDK)
    if(DEFINED ANDROID_PLATFORM_LEVEL)
        set(AURAW_ANDROID_MIN_SDK "${ANDROID_PLATFORM_LEVEL}")
    else()
        set(AURAW_ANDROID_MIN_SDK 26)
    endif()
endif()

if(NOT CMAKE_ANDROID_NDK AND DEFINED ANDROID_NDK)
    set(CMAKE_ANDROID_NDK "${ANDROID_NDK}")
endif()
if(NOT CMAKE_ANDROID_NDK)
    message(FATAL_ERROR "The active Android toolchain did not expose the NDK root")
endif()

if(DEFINED AURAW_LIBRAW_VERSION AND NOT AURAW_LIBRAW_VERSION STREQUAL "0.22.1")
    message(FATAL_ERROR
        "Cargo.toml requests LibRaw ${AURAW_LIBRAW_VERSION}, but this CMake pin is 0.22.1")
endif()
if(DEFINED AURAW_LENSFUN_VERSION AND NOT AURAW_LENSFUN_VERSION STREQUAL "0.3.4")
    message(FATAL_ERROR
        "Cargo.toml requests Lensfun ${AURAW_LENSFUN_VERSION}, but this CMake pin is 0.3.4")
endif()

set(CMAKE_POSITION_INDEPENDENT_CODE ON)
add_link_options(
    "-Wl,-z,max-page-size=16384"
    "-Wl,-z,common-page-size=16384"
)

ProcessorCount(AURAW_BUILD_JOBS)
if(NOT AURAW_BUILD_JOBS OR AURAW_BUILD_JOBS LESS 1)
    set(AURAW_BUILD_JOBS 1)
endif()

find_package(Python3 REQUIRED COMPONENTS Interpreter)
find_program(AURAW_MAKE_EXECUTABLE NAMES gmake make REQUIRED)
find_program(AURAW_PKG_CONFIG_EXECUTABLE NAMES pkg-config REQUIRED)

# Immutable source pins. URL hashes protect archive downloads in addition to the
# Git commit pins used by LibRaw, LibRaw-cmake, and Lensfun.
# Android pins CMake 3.22.1. Do not pass DOWNLOAD_EXTRACT_TIMESTAMP here:
# that option is not understood by 3.22 and is folded into URL_HASH, making
# configuration fail before any native dependency is downloaded.
FetchContent_Declare(
    auraw_libraw_source
    URL https://github.com/LibRaw/LibRaw/archive/b860248a89d9082b8e0a1e202e516f46af9adb29.tar.gz
    URL_HASH SHA256=f5da1e522ea195b54b30f3ff105ef2193daa04ea165dea825b4d6fe9d886395b
    SOURCE_SUBDIR _auraw_do_not_add
)
FetchContent_Declare(
    auraw_libraw_cmake
    URL https://github.com/LibRaw/LibRaw-cmake/archive/eb98e4325aef2ce85d2eb031c2ff18640ca616d3.tar.gz
    URL_HASH SHA256=3cd218bf6d1254de86e27269541277fbfc5bae57a9002ce0b46fbe2a97088b43
    SOURCE_SUBDIR _auraw_do_not_add
)
FetchContent_Declare(
    auraw_lensfun_source
    URL https://github.com/lensfun/lensfun/archive/101c745e847a5de4a1e569a94368ce2027198598.tar.gz
    URL_HASH SHA256=a11cbe6aeec657839540448b253217c25d20b7a45b6aebfef406f7239933c7a6
    SOURCE_SUBDIR _auraw_do_not_add
)
# GLib uses Meson. Fetch Meson's pinned source and invoke meson.py directly;
# this avoids the old project-local Python virtualenv and pip bootstrap.
FetchContent_Declare(
    auraw_meson_source
    URL https://files.pythonhosted.org/packages/02/98/bbcaf6caaaa0510a68834f119ac793a8abade6ff827fc2791eeb6f8b4a66/meson-1.7.0.tar.gz
    URL_HASH SHA256=08efbe84803eed07f863b05092d653a9d348f7038761d900412fddf56deb0284
    SOURCE_SUBDIR _auraw_do_not_add
)
FetchContent_MakeAvailable(
    auraw_libraw_source
    auraw_libraw_cmake
    auraw_lensfun_source
    auraw_meson_source
)

set(AURAW_ABI_STAGE "${AURAW_NATIVE_STAGE_ROOT}")
set(AURAW_LIBRAW_STAGE "${AURAW_ABI_STAGE}/libraw/${ANDROID_ABI}")
set(AURAW_LENSFUN_STAGE "${AURAW_ABI_STAGE}/lensfun/${ANDROID_ABI}")
set(AURAW_EXTERNAL_ROOT "${CMAKE_BINARY_DIR}/auraw-external")
file(MAKE_DIRECTORY
    "${AURAW_LIBRAW_STAGE}/include/libraw"
    "${AURAW_LIBRAW_STAGE}/lib"
    "${AURAW_LENSFUN_STAGE}/include"
    "${AURAW_LENSFUN_STAGE}/lib/pkgconfig"
)

# -----------------------------------------------------------------------------
# LibRaw 0.22.1
# -----------------------------------------------------------------------------
# Upstream LibRaw does not ship the CMake project used by Android, so overlay the
# separately pinned LibRaw-cmake files onto FetchContent's private source tree.
file(COPY "${auraw_libraw_cmake_SOURCE_DIR}/CMakeLists.txt"
     DESTINATION "${auraw_libraw_source_SOURCE_DIR}")
file(REMOVE_RECURSE "${auraw_libraw_source_SOURCE_DIR}/cmake")
file(COPY "${auraw_libraw_cmake_SOURCE_DIR}/cmake"
     DESTINATION "${auraw_libraw_source_SOURCE_DIR}")

set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(ENABLE_OPENMP OFF CACHE BOOL "" FORCE)
set(ENABLE_LCMS OFF CACHE BOOL "" FORCE)
set(ENABLE_JASPER OFF CACHE BOOL "" FORCE)
set(ENABLE_EXAMPLES OFF CACHE BOOL "" FORCE)
set(ENABLE_RAWSPEED OFF CACHE BOOL "" FORCE)
set(ENABLE_X3FTOOLS OFF CACHE BOOL "" FORCE)
set(LIBRAW_INSTALL OFF CACHE BOOL "" FORCE)
set(LIBRAW_UNINSTALL_TARGET OFF CACHE BOOL "" FORCE)
set(CMAKE_DISABLE_FIND_PACKAGE_JPEG ON CACHE BOOL "" FORCE)

add_subdirectory(
    "${auraw_libraw_source_SOURCE_DIR}"
    "${CMAKE_BINARY_DIR}/auraw-libraw"
    EXCLUDE_FROM_ALL
)
if(NOT TARGET raw)
    message(FATAL_ERROR "Pinned LibRaw-cmake did not define the expected 'raw' target")
endif()
set_property(TARGET raw PROPERTY POSITION_INDEPENDENT_CODE ON)

add_custom_target(auraw_stage_libraw
    COMMAND "${CMAKE_COMMAND}" -E make_directory
            "${AURAW_LIBRAW_STAGE}/include/libraw"
            "${AURAW_LIBRAW_STAGE}/lib"
    COMMAND "${CMAKE_COMMAND}" -E copy_directory
            "${auraw_libraw_source_SOURCE_DIR}/libraw"
            "${AURAW_LIBRAW_STAGE}/include/libraw"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
            "$<TARGET_FILE:raw>"
            "${AURAW_LIBRAW_STAGE}/lib/libraw.a"
    DEPENDS raw
    BYPRODUCTS
        "${AURAW_LIBRAW_STAGE}/include/libraw/libraw.h"
        "${AURAW_LIBRAW_STAGE}/lib/libraw.a"
    VERBATIM
)

# -----------------------------------------------------------------------------
# Lensfun 0.3.4 and its mandatory core GLib dependency
# -----------------------------------------------------------------------------
# Lensfun's BUILD_TESTS/BUILD_LENSTOOL/etc. switches remove optional consumers,
# but Lensfun 0.3.4 still calls find_package(GLIB2 REQUIRED) for the core library.
# Build the smallest pinned static GLib stack as CMake ExternalProject targets.
if(ANDROID_ABI STREQUAL "arm64-v8a")
    set(AURAW_CLANG_TARGET "aarch64-linux-android${AURAW_ANDROID_MIN_SDK}")
    set(AURAW_AUTOCONF_HOST "aarch64-linux-android")
    set(AURAW_MESON_CPU_FAMILY "aarch64")
    set(AURAW_MESON_CPU "aarch64")
endif()

get_filename_component(AURAW_NDK_TOOLCHAIN_BIN "${CMAKE_C_COMPILER}" DIRECTORY)
set(AURAW_NDK_CC "${AURAW_NDK_TOOLCHAIN_BIN}/${AURAW_CLANG_TARGET}-clang")
set(AURAW_NDK_CXX "${AURAW_NDK_TOOLCHAIN_BIN}/${AURAW_CLANG_TARGET}-clang++")
set(AURAW_NDK_AR "${AURAW_NDK_TOOLCHAIN_BIN}/llvm-ar")
set(AURAW_NDK_RANLIB "${AURAW_NDK_TOOLCHAIN_BIN}/llvm-ranlib")
set(AURAW_NDK_STRIP "${AURAW_NDK_TOOLCHAIN_BIN}/llvm-strip")
foreach(tool IN ITEMS
        AURAW_NDK_CC AURAW_NDK_CXX AURAW_NDK_AR AURAW_NDK_RANLIB AURAW_NDK_STRIP)
    if(NOT EXISTS "${${tool}}")
        message(FATAL_ERROR "Android NDK tool is missing: ${${tool}}")
    endif()
endforeach()

file(TO_CMAKE_PATH "${AURAW_LENSFUN_STAGE}" AURAW_LENSFUN_STAGE_MESON)
file(TO_CMAKE_PATH "${AURAW_PKG_CONFIG_EXECUTABLE}" AURAW_PKG_CONFIG_MESON)
file(TO_CMAKE_PATH "${AURAW_NDK_CC}" AURAW_NDK_CC_MESON)
file(TO_CMAKE_PATH "${AURAW_NDK_CXX}" AURAW_NDK_CXX_MESON)
file(TO_CMAKE_PATH "${AURAW_NDK_AR}" AURAW_NDK_AR_MESON)
file(TO_CMAKE_PATH "${AURAW_NDK_STRIP}" AURAW_NDK_STRIP_MESON)
set(AURAW_GLIB_CROSS_FILE "${AURAW_EXTERNAL_ROOT}/glib-${ANDROID_ABI}.cross")
file(MAKE_DIRECTORY "${AURAW_EXTERNAL_ROOT}")
file(WRITE "${AURAW_GLIB_CROSS_FILE}"
"[binaries]\n"
"c = '${AURAW_NDK_CC_MESON}'\n"
"cpp = '${AURAW_NDK_CXX_MESON}'\n"
"ar = '${AURAW_NDK_AR_MESON}'\n"
"strip = '${AURAW_NDK_STRIP_MESON}'\n"
"pkg-config = '${AURAW_PKG_CONFIG_MESON}'\n\n"
"[properties]\n"
"needs_exe_wrapper = true\n\n"
"[built-in options]\n"
"c_args = ['-fPIC', '-I${AURAW_LENSFUN_STAGE_MESON}/include']\n"
"cpp_args = ['-fPIC', '-I${AURAW_LENSFUN_STAGE_MESON}/include']\n"
"c_link_args = ['-L${AURAW_LENSFUN_STAGE_MESON}/lib', '-Wl,-z,max-page-size=16384', '-Wl,-z,common-page-size=16384']\n"
"cpp_link_args = ['-L${AURAW_LENSFUN_STAGE_MESON}/lib', '-Wl,-z,max-page-size=16384', '-Wl,-z,common-page-size=16384']\n\n"
"[host_machine]\n"
"system = 'android'\n"
"cpu_family = '${AURAW_MESON_CPU_FAMILY}'\n"
"cpu = '${AURAW_MESON_CPU}'\n"
"endian = 'little'\n")

# libiconv installs the iconv/charset archives consumed by GLib and Lensfun.
ExternalProject_Add(auraw_iconv
    URL
        https://ftpmirror.gnu.org/libiconv/libiconv-1.17.tar.gz
        https://ftp.gnu.org/pub/gnu/libiconv/libiconv-1.17.tar.gz
    URL_HASH SHA256=8f74213b56238c85a50a5329f77e06198771e70dd9a739779f4c02f65d971313
    PREFIX "${AURAW_EXTERNAL_ROOT}/iconv-prefix"
    BINARY_DIR "${AURAW_EXTERNAL_ROOT}/iconv-build"
    CONFIGURE_COMMAND
        "${CMAKE_COMMAND}" -E env
        "CC=${AURAW_NDK_CC}"
        "CXX=${AURAW_NDK_CXX}"
        "AR=${AURAW_NDK_AR}"
        "RANLIB=${AURAW_NDK_RANLIB}"
        "CFLAGS=-fPIC"
        "CXXFLAGS=-fPIC"
        "LDFLAGS=-Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384"
        <SOURCE_DIR>/configure
        "--host=${AURAW_AUTOCONF_HOST}"
        "--prefix=${AURAW_LENSFUN_STAGE}"
        --disable-shared
        --enable-static
        --with-pic
    BUILD_COMMAND "${AURAW_MAKE_EXECUTABLE}" "-j${AURAW_BUILD_JOBS}"
    INSTALL_COMMAND "${AURAW_MAKE_EXECUTABLE}" "-j${AURAW_BUILD_JOBS}" install
    BUILD_BYPRODUCTS
        "${AURAW_LENSFUN_STAGE}/lib/libiconv.a"
        "${AURAW_LENSFUN_STAGE}/lib/libcharset.a"
        "${AURAW_LENSFUN_STAGE}/lib/libintl.a"
)

file(WRITE "${AURAW_LENSFUN_STAGE}/lib/pkgconfig/iconv.pc"
"prefix=${AURAW_LENSFUN_STAGE}\n"
"libdir=\${prefix}/lib\n\n"
"Name: iconv\n"
"Description: GNU libiconv for AuRaw Android Lensfun\n"
"Version: 1.17\n"
"Libs: -L\${libdir} -liconv -lcharset\n")

# GLib 2.78's Meson fallback for zlib 1.2.11 downloads from
# zlib.net/fossils. That endpoint is unreliable in CI, so build a newer pinned
# zlib from its official GitHub release and expose it through the isolated
# Android pkg-config directory used below. This also keeps Meson from ever
# resolving a zlib from the build host.
ExternalProject_Add(auraw_zlib
    URL https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz
    URL_HASH SHA256=9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23
    PREFIX "${AURAW_EXTERNAL_ROOT}/zlib-prefix"
    BINARY_DIR "${AURAW_EXTERNAL_ROOT}/zlib-build"
    CONFIGURE_COMMAND
        "${CMAKE_COMMAND}"
        -S <SOURCE_DIR>
        -B <BINARY_DIR>
        -G Ninja
        "-DCMAKE_MAKE_PROGRAM=${CMAKE_MAKE_PROGRAM}"
        "-DCMAKE_TOOLCHAIN_FILE=${CMAKE_TOOLCHAIN_FILE}"
        "-DANDROID_NDK=${CMAKE_ANDROID_NDK}"
        "-DANDROID_ABI=${ANDROID_ABI}"
        "-DANDROID_PLATFORM=android-${AURAW_ANDROID_MIN_SDK}"
        -DANDROID_STL=c++_shared
        -DCMAKE_BUILD_TYPE=Release
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON
        -DZLIB_BUILD_EXAMPLES=OFF
    BUILD_COMMAND
        "${CMAKE_COMMAND}" --build <BINARY_DIR> --target zlibstatic
        --parallel "${AURAW_BUILD_JOBS}"
    INSTALL_COMMAND
        "${CMAKE_COMMAND}" -E rm -f
        "${AURAW_LENSFUN_STAGE}/lib/libz.so"
        "${AURAW_LENSFUN_STAGE}/lib/libz.so.1"
        "${AURAW_LENSFUN_STAGE}/lib/libz.so.1.3.1"
        COMMAND "${CMAKE_COMMAND}" -E copy_if_different
        <BINARY_DIR>/libz.a "${AURAW_LENSFUN_STAGE}/lib/libz.a"
        COMMAND "${CMAKE_COMMAND}" -E copy_if_different
        <SOURCE_DIR>/zlib.h "${AURAW_LENSFUN_STAGE}/include/zlib.h"
        COMMAND "${CMAKE_COMMAND}" -E copy_if_different
        <BINARY_DIR>/zconf.h "${AURAW_LENSFUN_STAGE}/include/zconf.h"
    BUILD_BYPRODUCTS
        "${AURAW_LENSFUN_STAGE}/include/zlib.h"
        "${AURAW_LENSFUN_STAGE}/lib/libz.a"
        "${AURAW_LENSFUN_STAGE}/lib/pkgconfig/zlib.pc"
)

file(WRITE "${AURAW_LENSFUN_STAGE}/lib/pkgconfig/zlib.pc"
"prefix=${AURAW_LENSFUN_STAGE}\n"
"includedir=\${prefix}/include\n"
"libdir=\${prefix}/lib\n\n"
"Name: zlib\n"
"Description: zlib compression library for AuRaw Android GLib\n"
"Version: 1.3.1\n"
"Libs: -L\${libdir} -lz\n"
"Cflags: -I\${includedir}\n")

set(AURAW_PKG_CONFIG_DIR "${AURAW_LENSFUN_STAGE}/lib/pkgconfig")
set(AURAW_MESON "${Python3_EXECUTABLE};${auraw_meson_source_SOURCE_DIR}/meson.py")
ExternalProject_Add(auraw_glib
    URL https://download.gnome.org/sources/glib/2.78/glib-2.78.6.tar.xz
    URL_HASH SHA256=244854654dd82c7ebcb2f8e246156d2a05eb9cd1ad07ed7a779659b4602c9fae
    PREFIX "${AURAW_EXTERNAL_ROOT}/glib-prefix"
    BINARY_DIR "${AURAW_EXTERNAL_ROOT}/glib-build"
    DEPENDS auraw_iconv auraw_zlib
    CONFIGURE_COMMAND
        "${CMAKE_COMMAND}" -E env
        "PKG_CONFIG_LIBDIR=${AURAW_PKG_CONFIG_DIR}"
        "PKG_CONFIG_PATH=${AURAW_PKG_CONFIG_DIR}"
        ${AURAW_MESON}
        setup
        <BINARY_DIR>
        <SOURCE_DIR>
        --cross-file "${AURAW_GLIB_CROSS_FILE}"
        --wrap-mode=default
        --force-fallback-for=libpcre2-8,libffi
        --prefix "${AURAW_LENSFUN_STAGE}"
        --libdir lib
        --default-library static
        --buildtype release
        -Dtests=false
        -Dnls=disabled
        -Dglib_debug=disabled
        -Dglib_assert=false
        -Dglib_checks=false
        -Dselinux=disabled
        -Dxattr=false
        -Dlibmount=disabled
        -Dman=false
        -Dgtk_doc=false
    BUILD_COMMAND
        "${CMAKE_COMMAND}" -E env
        "PKG_CONFIG_LIBDIR=${AURAW_PKG_CONFIG_DIR}"
        "PKG_CONFIG_PATH=${AURAW_PKG_CONFIG_DIR}"
        ${AURAW_MESON} compile -C <BINARY_DIR>
    INSTALL_COMMAND
        "${CMAKE_COMMAND}" -E env
        "PKG_CONFIG_LIBDIR=${AURAW_PKG_CONFIG_DIR}"
        "PKG_CONFIG_PATH=${AURAW_PKG_CONFIG_DIR}"
        ${AURAW_MESON} install -C <BINARY_DIR>
    BUILD_BYPRODUCTS
        "${AURAW_LENSFUN_STAGE}/lib/libglib-2.0.a"
        "${AURAW_LENSFUN_STAGE}/lib/libpcre2-8.a"
        "${AURAW_LENSFUN_STAGE}/lib/libffi.a"
)

ExternalProject_Add(auraw_lensfun
    SOURCE_DIR "${auraw_lensfun_source_SOURCE_DIR}"
    DOWNLOAD_COMMAND ""
    UPDATE_COMMAND ""
    PREFIX "${AURAW_EXTERNAL_ROOT}/lensfun-prefix"
    BINARY_DIR "${AURAW_EXTERNAL_ROOT}/lensfun-build"
    DEPENDS auraw_glib
    CONFIGURE_COMMAND
        "${CMAKE_COMMAND}" -E env
        "PKG_CONFIG_LIBDIR=${AURAW_PKG_CONFIG_DIR}"
        "PKG_CONFIG_PATH=${AURAW_PKG_CONFIG_DIR}"
        "${CMAKE_COMMAND}"
        -S <SOURCE_DIR>
        -B <BINARY_DIR>
        -G Ninja
        "-DCMAKE_MAKE_PROGRAM=${CMAKE_MAKE_PROGRAM}"
        "-DCMAKE_TOOLCHAIN_FILE=${CMAKE_TOOLCHAIN_FILE}"
        "-DANDROID_NDK=${CMAKE_ANDROID_NDK}"
        "-DANDROID_ABI=${ANDROID_ABI}"
        "-DANDROID_PLATFORM=android-${AURAW_ANDROID_MIN_SDK}"
        -DANDROID_STL=c++_shared
        -DCMAKE_BUILD_TYPE=Release
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON
        "-DCMAKE_PREFIX_PATH=${AURAW_LENSFUN_STAGE}"
        "-DCMAKE_INSTALL_PREFIX=${AURAW_LENSFUN_STAGE}"
        -DCMAKE_INSTALL_LIBDIR=lib
        -DCMAKE_INSTALL_DATAROOTDIR=apk-assets
        "-DPKG_CONFIG_EXECUTABLE=${AURAW_PKG_CONFIG_EXECUTABLE}"
        -DBUILD_STATIC=ON
        -DBUILD_TESTS=OFF
        -DBUILD_LENSTOOL=OFF
        -DBUILD_DOC=OFF
        -DINSTALL_PYTHON_MODULE=OFF
        -DINSTALL_HELPER_SCRIPTS=OFF
        -DBUILD_FOR_SSE=OFF
        -DBUILD_FOR_SSE2=OFF
        "-DCMAKE_SHARED_LINKER_FLAGS=-Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384"
        "-DCMAKE_EXE_LINKER_FLAGS=-Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384"
    BUILD_COMMAND
        "${CMAKE_COMMAND}" --build <BINARY_DIR> --target install --parallel "${AURAW_BUILD_JOBS}"
    INSTALL_COMMAND ""
    BUILD_BYPRODUCTS
        "${AURAW_LENSFUN_STAGE}/include/lensfun/lensfun.h"
        "${AURAW_LENSFUN_STAGE}/lib/liblensfun.a"
)

add_custom_target(auraw_verify_lensfun
    COMMAND "${CMAKE_COMMAND}"
            "-DSTAGE_ROOT=${AURAW_LENSFUN_STAGE}"
            -P "${CMAKE_CURRENT_LIST_DIR}/cmake/VerifyLensfunStage.cmake"
    DEPENDS auraw_lensfun
    VERBATIM
)

# Targets available to a future C++ JNI target in this project. The existing
# Rust cdylib consumes the same staged archives through auraw-core/build.rs.
add_library(auraw_lensfun_static STATIC IMPORTED GLOBAL)
set_target_properties(auraw_lensfun_static PROPERTIES
    IMPORTED_LOCATION "${AURAW_LENSFUN_STAGE}/lib/liblensfun.a"
    INTERFACE_INCLUDE_DIRECTORIES "${AURAW_LENSFUN_STAGE}/include"
)
add_dependencies(auraw_lensfun_static auraw_verify_lensfun)
add_library(auraw::lensfun ALIAS auraw_lensfun_static)
add_library(auraw::libraw ALIAS raw)

add_library(auraw_native_static INTERFACE)
target_link_libraries(auraw_native_static INTERFACE raw auraw_lensfun_static)
add_dependencies(auraw_native_static auraw_stage_libraw auraw_verify_lensfun)
add_library(auraw::native_static ALIAS auraw_native_static)

# AGP builds this explicit target for every ABI in ndk.abiFilters.
add_custom_target(auraw_native_deps ALL)
add_dependencies(auraw_native_deps auraw_stage_libraw auraw_verify_lensfun)
