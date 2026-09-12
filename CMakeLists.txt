cmake_minimum_required(VERSION 3.16)
project(wash LANGUAGES CXX)

# 使用 C++17
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# 查找 readline 库
find_package(Readline REQUIRED)

# 源文件
set(SOURCES
    src/main.cpp
    src/lexer.cpp
    src/parser.cpp
    src/executor.cpp
)

# 头文件目录
set(HEADERS
    src/types.h
    src/lexer.h
    src/parser.h
    src/executor.h
)

# 构建可执行文件
add_executable(wash ${SOURCES} ${HEADERS})

# 包含头文件目录
target_include_directories(wash PRIVATE ${CMAKE_SOURCE_DIR}/src)

# 链接 readline 库
target_link_libraries(wash PRIVATE Readline::Readline)

# 安装配置
install(TARGETS wash DESTINATION bin)
