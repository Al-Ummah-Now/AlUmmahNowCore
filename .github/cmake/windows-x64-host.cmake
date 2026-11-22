# Host toolchain for ggml's Vulkan shader generator when aun-core is cross-built for Windows ARM64
# (passed as GGML_VULKAN_SHADERS_GEN_TOOLCHAIN by build.yml). The generator runs on the x64 build
# machine, so it is built with the x64 MSVC compiler and x64 libraries, inside the x64-to-ARM64
# developer environment that msvc-dev-cmd sets up (VCToolsInstallDir, WindowsSdkDir, ...).
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR AMD64)
file(TO_CMAKE_PATH "$ENV{VCToolsInstallDir}" AUN_VC)
file(TO_CMAKE_PATH "$ENV{WindowsSdkDir}" AUN_SDK)
set(AUN_SDK_VER "$ENV{WindowsSDKLibVersion}")
string(REGEX REPLACE "[\\/]+$" "" AUN_SDK_VER "${AUN_SDK_VER}")
set(CMAKE_C_COMPILER "${AUN_VC}/bin/HostX64/x64/cl.exe")
set(CMAKE_CXX_COMPILER "${AUN_VC}/bin/HostX64/x64/cl.exe")
set(CMAKE_CXX_FLAGS_INIT "/EHsc")
# x64 libraries first: the environment's LIB lists the ARM64 ones
set(AUN_X64_LIBS "/LIBPATH:\"${AUN_VC}/lib/x64\" /LIBPATH:\"${AUN_SDK}/Lib/${AUN_SDK_VER}/um/x64\" /LIBPATH:\"${AUN_SDK}/Lib/${AUN_SDK_VER}/ucrt/x64\"")
set(CMAKE_EXE_LINKER_FLAGS_INIT "${AUN_X64_LIBS}")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "${AUN_X64_LIBS}")
set(CMAKE_MODULE_LINKER_FLAGS_INIT "${AUN_X64_LIBS}")
