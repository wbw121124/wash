cmake_minimum_required(VERSION 3.16)
project(wash LANGUAGES CXX VERSION 2.0.0)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# 平台检测
if(WIN32)
    add_compile_definitions(_WIN32 WIN32)
    if(MINGW)
        add_compile_definitions(__MSYS__ __MINGW32__ __MINGW64__)
    endif()
elseif(UNIX)
    add_compile_definitions(_GNU_SOURCE _POSIX_C_SOURCE=200809L)
endif()

# 源文件
set(SOURCES
    src/main.cpp
    src/lexer.cpp
    src/parser.cpp
    src/executor.cpp
    src/color.cpp
)

set(HEADERS
    src/types.h
    src/lexer.h
    src/parser.h
    src/executor.h
    src/color.h
)

# 主程序
add_executable(wash ${SOURCES} ${HEADERS})
target_include_directories(wash PRIVATE ${CMAKE_SOURCE_DIR}/src)

# 跨平台链接库
if(WIN32)
    target_link_libraries(wash PRIVATE readline intl ncurses)
else()
    # Linux: libintl is part of glibc, no need to link separately
    target_link_libraries(wash PRIVATE readline ncurses)
endif()

# 测试目标
add_executable(wash_test test/test_phase2.cpp src/lexer.cpp src/parser.cpp src/executor.cpp src/color.cpp)
target_include_directories(wash_test PRIVATE ${CMAKE_SOURCE_DIR}/src)
if(WIN32)
    target_link_libraries(wash_test PRIVATE readline intl ncurses)
else()
    target_link_libraries(wash_test PRIVATE readline ncurses)
endif()

add_executable(wash_test3 test/test_phase3.cpp src/lexer.cpp src/parser.cpp src/executor.cpp src/color.cpp)
target_include_directories(wash_test3 PRIVATE ${CMAKE_SOURCE_DIR}/src)
if(WIN32)
    target_link_libraries(wash_test3 PRIVATE readline intl ncurses)
else()
    target_link_libraries(wash_test3 PRIVATE readline ncurses)
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
        get_filename_component(LANG ${PO_FILE} NAME_WE)
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
