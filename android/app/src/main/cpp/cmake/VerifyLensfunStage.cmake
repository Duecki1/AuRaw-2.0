if(NOT DEFINED STAGE_ROOT)
    message(FATAL_ERROR "STAGE_ROOT is required")
endif()

set(required_files
    "include/lensfun/lensfun.h"
    "lib/liblensfun.a"
    "lib/libiconv.a"
    "lib/libcharset.a"
    "lib/libglib-2.0.a"
    "lib/libpcre2-8.a"
    "lib/libffi.a"
    "lib/libz.a"
    "lib/libintl.a"
)
foreach(relative_path IN LISTS required_files)
    if(NOT EXISTS "${STAGE_ROOT}/${relative_path}")
        message(FATAL_ERROR
            "Lensfun native stage is incomplete: ${STAGE_ROOT}/${relative_path}")
    endif()
endforeach()

file(GLOB_RECURSE lensfun_xml "${STAGE_ROOT}/apk-assets/lensfun/*.xml")
if(NOT lensfun_xml)
    message(FATAL_ERROR
        "Lensfun profile XML was not installed under ${STAGE_ROOT}/apk-assets/lensfun")
endif()
