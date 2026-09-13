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
    src/color.cpp
)

set(HEADERS
    src/types.h
    src/lexer.h
    src/parser.h
    src/executor.h
    src/color.h
)

add_executable(wash ${SOURCES} ${HEADERS})
target_include_directories(wash PRIVATE ${CMAKE_SOURCE_DIR}/src)
target_link_libraries(wash PRIVATE readline intl ncurses)

# 测试目标
add_executable(wash_test test/test_phase2.cpp src/lexer.cpp src/parser.cpp src/executor.cpp src/color.cpp)
target_include_directories(wash_test PRIVATE ${CMAKE_SOURCE_DIR}/src)
target_link_libraries(wash_test PRIVATE readline intl ncurses)

add_executable(wash_test3 test/test_phase3.cpp src/lexer.cpp src/parser.cpp src/executor.cpp src/color.cpp)
target_include_directories(wash_test3 PRIVATE ${CMAKE_SOURCE_DIR}/src)
target_link_libraries(wash_test3 PRIVATE readline intl ncurses)

# i18n: 编译 .po 文件到 .mo 文件
find_program(GETTEXT_MSGFMT msgfmt)
if(GETTEXT_MSGFMT)
    file(GLOB PO_FILES po/*.po)
    foreach(PO_FILE ${PO_FILES})
        get_filename_component(LANG ${PO_FILE} NAME_WE)
        set(MO_DIR ${CMAKE_SOURCE_DIR}/share/locale/${LANG}/LC_MESSAGES)
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
endif()
