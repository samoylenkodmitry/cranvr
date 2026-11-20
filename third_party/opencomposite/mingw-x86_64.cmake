# The cross toolchain that builds BasaltVR's runtime (MinGW-w64 from Homebrew), for a Windows x64 DLL
# that runs under Wine.
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR x86_64)

find_program(OC_MINGW_C x86_64-w64-mingw32-gcc REQUIRED)
find_program(OC_MINGW_CXX x86_64-w64-mingw32-g++ REQUIRED)
find_program(OC_MINGW_RC x86_64-w64-mingw32-windres REQUIRED)

set(CMAKE_C_COMPILER "${OC_MINGW_C}" CACHE FILEPATH "")
set(CMAKE_CXX_COMPILER "${OC_MINGW_CXX}" CACHE FILEPATH "")
set(CMAKE_RC_COMPILER "${OC_MINGW_RC}" CACHE FILEPATH "")

set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

# OpenComposite tests WIN32, which MSVC defines and GCC does not define in strict -std=c++20 mode
set(CMAKE_C_FLAGS_INIT "-DWIN32 -D_WINDOWS")
# GCC 16's standard headers no longer include <cstdint> for the OpenXR loader's sources. MinGW-w64
# has no ATL: shim/atlbase.h holds the part that OpenComposite uses.
set(CMAKE_CXX_FLAGS_INIT "-DWIN32 -D_WINDOWS -include cstdint -I${CMAKE_CURRENT_LIST_DIR}/shim")
