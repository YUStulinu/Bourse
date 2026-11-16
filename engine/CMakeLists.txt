cmake_minimum_required(VERSION 3.20)
project(bourse_engine CXX)
add_compile_definitions(_CRT_SECURE_NO_WARNINGS)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()
if(CMAKE_CXX_COMPILER_ID MATCHES "Clang|GNU")
  add_compile_options(-Wall -Wextra -march=native)
endif()

# MinGW: link the C++ runtime statically, so the programs and the Python
# library run without MinGW's DLLs on the path
if(MINGW)
  add_link_options(-static)
endif()

find_package(Threads REQUIRED)

add_library(bourse_core STATIC src/bus.cpp src/loop.cpp)
target_include_directories(bourse_core PUBLIC include)
target_link_libraries(bourse_core PUBLIC Threads::Threads)

add_executable(bourse-engine src/engine_main.cpp)
target_link_libraries(bourse-engine bourse_core)

# the engine for Python (ctypes)
add_library(bourse SHARED src/capi.cpp)
target_include_directories(bourse PRIVATE include)

add_executable(itch-replay tools/itch_replay.cpp)
target_link_libraries(itch-replay bourse_core)

add_executable(engine-bench tools/bench.cpp)
target_link_libraries(engine-bench bourse_core)

add_executable(engine-tests tests/test_engine.cpp)
target_link_libraries(engine-tests bourse_core)

enable_testing()
add_test(NAME engine-tests COMMAND engine-tests)
