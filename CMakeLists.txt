cmake_minimum_required(VERSION 3.16)
project(wash LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# 启用 POSIX 特性
add_compile_definitions(_GNU_SOURCE _POSIX_C_SOURCE=200809L)

set(SOURCES
    src/main.cpp
    src/lexer.cpp
    src/parser.cpp
    src/executor.cpp
)

set(HEADERS
    src/types.h
    src/lexer.h
    src/parser.h
    src/executor.h
)

add_executable(wash ${SOURCES} ${HEADERS})
target_include_directories(wash PRIVATE ${CMAKE_SOURCE_DIR}/src)
target_link_libraries(wash PRIVATE readline)
