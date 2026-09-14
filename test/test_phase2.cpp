/**
 * @file test_phase2.cpp
 * @brief Phase 2 功能单元测试
 */

#include <iostream>
#include <cassert>
#include <string>
#include <cstdio>
#include <cstdlib>
#include "compat.h"

// 执行 wash 脚本文件并返回 stdout 输出
std::string runWash(const std::string& script) {
    std::string tmpFile = "test_tmp.wash";
    FILE* f = fopen(tmpFile.c_str(), "w");
    fprintf(f, "%s\n", script.c_str());
    fflush(f);
    fclose(f);
    
    std::string exeDir = wash::compat::getExeDir();
    std::string cmd = exeDir + "/wash.exe " + tmpFile;
    
    FILE* pipe = wash::compat::popenCommand(cmd, "r");
    std::string output;
    char buf[4096];
    while (fgets(buf, sizeof(buf), pipe)) output += buf;
    wash::compat::pcloseCommand(pipe);
    remove(tmpFile.c_str());
    
    while (!output.empty() && (output.back() == '\n' || output.back() == '\r')) output.pop_back();
    return output;
}

void test_basic_assignment() {
    assert(runWash("%x = 10; echo(%x)") == "10");
    std::cout << "[PASS] test_basic_assignment" << std::endl;
}

void test_string_assignment() {
    assert(runWash("%name = \"wash\"; echo(%name)") == "wash");
    std::cout << "[PASS] test_string_assignment" << std::endl;
}

void test_calc_expression() {
    assert(runWash("%result = calc(2 + 3); echo(%result)") == "5");
    std::cout << "[PASS] test_calc_expression" << std::endl;
}

void test_if_else() {
    assert(runWash("%x = 10; if %x > 5 { echo(\"big\") } else { echo(\"small\") }") == "big");
    std::cout << "[PASS] test_if_else" << std::endl;
}

void test_for_loop() {
    assert(runWash("for %i in 1..3 { echo(%i) }") == "1\n2\n3");
    std::cout << "[PASS] test_for_loop" << std::endl;
}

void test_while_loop() {
    assert(runWash("%x = 0; while %x < 3 { %x = %x + 1; echo(%x) }") == "1\n2\n3");
    std::cout << "[PASS] test_while_loop" << std::endl;
}

void test_function_def() {
    assert(runWash("fn add(%a, %b) { return(calc(%a + %b)) }; %result = add(2, 3); echo(%result)") == "5");
    std::cout << "[PASS] test_function_def" << std::endl;
}

void test_lambda() {
    assert(runWash("%double = fn(%x) { return(calc(%x * 2)) }; %result = %double(5); echo(%result)") == "10");
    std::cout << "[PASS] test_lambda" << std::endl;
}

void test_string_interpolation() {
    assert(runWash("%name = \"wash\"; echo(\"hello %name\")") == "hello wash");
    std::cout << "[PASS] test_string_interpolation" << std::endl;
}

void test_array_literal() {
    assert(runWash("%arr = [1, 2, 3]; echo(%arr)") == "[1, 2, 3]");
    std::cout << "[PASS] test_array_literal" << std::endl;
}

void test_array_access() {
    assert(runWash("%arr = [10, 20, 30]; echo(%arr[1])") == "20");
    std::cout << "[PASS] test_array_access" << std::endl;
}

void test_nested_if() {
    assert(runWash("%x = 10; if %x > 5 { if %x > 8 { echo(\"deep\") } }") == "deep");
    std::cout << "[PASS] test_nested_if" << std::endl;
}

void test_ternary() {
    assert(runWash("%x = 10; %result = %x > 5 ? 1 : 0; echo(%result)") == "1");
    std::cout << "[PASS] test_ternary" << std::endl;
}

void test_command_output() {
    std::string result = runWash("echo($(echo hello))");
    assert(result == "hello");
    std::cout << "[PASS] test_command_output" << std::endl;
}

void test_break_continue() {
    assert(runWash("for %i in 1..10 { if %i > 3 { break() }; echo(%i) }") == "1\n2\n3");
    std::cout << "[PASS] test_break_continue" << std::endl;
}

void test_comma_expression() {
    assert(runWash("%x = (1, 2, 3); echo(%x)") == "3");
    std::cout << "[PASS] test_comma_expression" << std::endl;
}

void test_complex_calc() {
    assert(runWash("%result = calc((2 + 3) * 4); echo(%result)") == "20");
    std::cout << "[PASS] test_complex_calc" << std::endl;
}

void test_variable_reassignment() {
    assert(runWash("%x = 1; %x = 2; echo(%x)") == "2");
    std::cout << "[PASS] test_variable_reassignment" << std::endl;
}

void test_string_concat() {
    assert(runWash("%a = \"hello\"; %b = \" world\"; echo(%a + %b)") == "hello world");
    std::cout << "[PASS] test_string_concat" << std::endl;
}

int main() {
    std::cout << "=== Phase 2 Tests ===" << std::endl;
    
    test_basic_assignment();
    test_string_assignment();
    test_calc_expression();
    test_if_else();
    test_for_loop();
    test_while_loop();
    test_function_def();
    test_lambda();
    test_string_interpolation();
    test_array_literal();
    test_array_access();
    test_nested_if();
    test_ternary();
    test_command_output();
    test_break_continue();
    test_comma_expression();
    test_complex_calc();
    test_variable_reassignment();
    test_string_concat();
    
    std::cout << "\n=== All 19 tests passed! ===" << std::endl;
    return 0;
}
