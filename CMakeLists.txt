cmake_minimum_required(VERSION 3.16)
project(wash LANGUAGES CXX)

# 使用 C++17
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# 远程依赖
# include(FetchContent)
# 免每次配置都检查远程更新
# set(FETCHCONTENT_UPDATES_DISCONNECTED TRUE)

add_executable(wash src/ain.cpp)
