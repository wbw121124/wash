cmake_minimum_required(VERSION 3.16)
project(wash LANGUAGES CXX VERSION 2.0.0)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# =====================================================================
# 平台检测
# =====================================================================
# 在 MSYS2 中使用 "Unix Makefiles" 生成器时，CMAKE_SYSTEM_NAME=MSYS，
# WIN32/MINGW 均为 false，但编译器可能是 UCRT64/MinGW64。
set(WASH_MINGW_ENV FALSE)
if(MINGW)
    set(WASH_MINGW_ENV TRUE)
elseif(CMAKE_CXX_COMPILER MATCHES "(ucrt64|mingw64|mingw32)")
    set(WASH_MINGW_ENV TRUE)
endif()

if(WASH_MINGW_ENV)
    # UCRT64 / MinGW64：MinGW 编译器已自动定义 __MINGW32__、__MINGW64__
    # 代码中通过 __MINGW32__ 优先走 POSIX 路径
elseif(UNIX)
    add_compile_definitions(_GNU_SOURCE _POSIX_C_SOURCE=200809L)
elseif(WIN32)
    add_compile_definitions(_WIN32 WIN32)
endif()

# =====================================================================
# 修复 MSYS2 UCRT64/MinGW64 的头文件冲突
# =====================================================================
# MSYS2 的 CMake 平台模块（UnixPaths.cmake）会自动把 /usr/include 加入
# CXX 隐式搜索路径，导致 MSYS 头文件（wchar.h/wctype.h/pthread.h 等）
# 与 UCRT64 头文件严重冲突（mbstate_t/wctype_t 重定义）。
if(WASH_MINGW_ENV)
    # 替换隐式搜索路径：用 /ucrt64/include 替代 /usr/include，
    # 防止 MSYS 头文件污染 UCRT64 编译，同时保留 C++ 标准库的 #include_next 链
    foreach(_dir IN LISTS CMAKE_CXX_IMPLICIT_INCLUDE_DIRECTORIES)
        if(_dir MATCHES "usr/include")
            list(REMOVE_ITEM CMAKE_CXX_IMPLICIT_INCLUDE_DIRECTORIES "${_dir}")
            list(APPEND CMAKE_CXX_IMPLICIT_INCLUDE_DIRECTORIES "/ucrt64/include")
        endif()
    endforeach()
    foreach(_dir IN LISTS CMAKE_C_IMPLICIT_INCLUDE_DIRECTORIES)
        if(_dir MATCHES "usr/include")
            list(REMOVE_ITEM CMAKE_C_IMPLICIT_INCLUDE_DIRECTORIES "${_dir}")
            list(APPEND CMAKE_C_IMPLICIT_INCLUDE_DIRECTORIES "/ucrt64/include")
        endif()
    endforeach()
endif()

# =====================================================================
# 查找 gettext/libintl
# =====================================================================
if(WASH_MINGW_ENV)
    # 手动创建 Intl::Intl imported target，仅引用 /ucrt64 的库和头文件
    # 避免 find_package(Intl) 找到 /usr/include（MSYS）引发头文件冲突
    add_library(Intl::Intl UNKNOWN IMPORTED)
    set_target_properties(Intl::Intl PROPERTIES
        IMPORTED_LOCATION "/ucrt64/lib/libintl.dll.a"
    )
    # UCRT64 ncurses 的 term.h/curses.h 在子目录中
    include_directories("/ucrt64/include/ncurses")
else()
    find_package(Intl REQUIRED)
endif()

# 源文件
set(SOURCES
    src/main.cpp
    src/lexer.cpp
    src/parser.cpp
    src/executor.cpp
    src/color.cpp
    src/compat.cpp
)

set(HEADERS
    src/types.h
    src/lexer.h
    src/parser.h
    src/executor.h
    src/color.h
    src/compat.h
)

# 主程序
add_executable(wash ${SOURCES} ${HEADERS})
target_include_directories(wash PRIVATE ${CMAKE_SOURCE_DIR}/src)

# 跨平台链接库
if(WASH_MINGW_ENV)
    target_link_libraries(wash PRIVATE readline Intl::Intl ncursesw)
else()
    target_link_libraries(wash PRIVATE readline Intl::Intl ncurses)
endif()

# 测试目标
add_executable(wash_test test/test_phase2.cpp src/lexer.cpp src/parser.cpp src/executor.cpp src/color.cpp src/compat.cpp)
target_include_directories(wash_test PRIVATE ${CMAKE_SOURCE_DIR}/src)
if(WASH_MINGW_ENV)
    target_link_libraries(wash_test PRIVATE readline Intl::Intl ncursesw)
else()
    target_link_libraries(wash_test PRIVATE readline Intl::Intl ncurses)
endif()

add_executable(wash_test3 test/test_phase3.cpp src/lexer.cpp src/parser.cpp src/executor.cpp src/color.cpp src/compat.cpp)
target_include_directories(wash_test3 PRIVATE ${CMAKE_SOURCE_DIR}/src)
if(WASH_MINGW_ENV)
    target_link_libraries(wash_test3 PRIVATE readline Intl::Intl ncursesw)
else()
    target_link_libraries(wash_test3 PRIVATE readline Intl::Intl ncurses)
endif()

# i18n: 编译 .po 文件到 .mo 文件
find_program(GETTEXT_MSGFMT msgfmt)
if(GETTEXT_MSGFMT)
    file(GLOB PO_FILES po/*.po)
    foreach(PO_FILE ${PO_FILES})
        get_filename_component(LANG ${PO_FILE} NAME_WE)
        set(MO_DIR ${CMAKE_BINARY_DIR}/share/locale/${LANG}/LC_MESSAGES)
        file(MAKE_DIRECTORY ${MO_DIR})
        set(MO_FILE ${MO_DIR}/wash.mo)
        add_custom_command(
            OUTPUT ${MO_FILE}
            COMMAND ${GETTEXT_MSGFMT} -o ${MO_FILE} ${PO_FILE}
            DEPENDS ${PO_FILE}
            COMMENT "Compiling ${LANG} translation"
        )
        list(APPEND MO_FILES ${MO_FILE})
    endforeach()
    add_custom_target(translations ALL DEPENDS ${MO_FILES})
    add_dependencies(wash translations)
endif()

# 安装目标
include(GNUInstallDirs)

# 安装可执行文件
install(TARGETS wash
    RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR}
)

# 安装翻译文件
if(GETTEXT_MSGFMT)
    foreach(PO_FILE ${PO_FILES})
        install(FILES ${CMAKE_BINARY_DIR}/share/locale/${LANG}/LC_MESSAGES/wash.mo
            DESTINATION ${CMAKE_INSTALL_LOCALEDIR}/${LANG}/LC_MESSAGES
        )
    endforeach()
endif()

# 安装文档
install(FILES README.md LICENSE
    DESTINATION ${CMAKE_INSTALL_DOCDIR}
)

# 安装示例
install(DIRECTORY examples
    DESTINATION ${CMAKE_INSTALL_DOCDIR}/wash
)

# 卸载目标（可选）
add_custom_target(uninstall
    COMMAND ${CMAKE_COMMAND} -E remove ${CMAKE_INSTALL_PREFIX}/bin/wash
    COMMAND ${CMAKE_COMMAND} -E remove_directory ${CMAKE_INSTALL_PREFIX}/share/locale/zh_CN/LC_MESSAGES
    COMMAND ${CMAKE_COMMAND} -E remove_directory ${CMAKE_INSTALL_PREFIX}/share/locale/en_US/LC_MESSAGES
    COMMENT "Uninstalling wash..."
)
