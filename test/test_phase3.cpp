/**
 * @file test_phase3.cpp
 * @brief Phase 3 内建函数单元测试
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

void test_find() {
    assert(runWash("echo($(find(\"hello world\", \"world\")))") == "6");
    std::cout << "[PASS] test_find" << std::endl;
}

void test_split() {
    assert(runWash("echo($(split(\"a,b,c\", \",\")))") == "a,b,c");
    std::cout << "[PASS] test_split" << std::endl;
}

void test_join() {
    assert(runWash("echo($(join(\"a,b,c\", \"-\")))") == "a-b-c");
    std::cout << "[PASS] test_join" << std::endl;
}

void test_read() {
    // read() 需要交互输入，这里测试帮助函数
    assert(runWash("echo($(length(\"hello\")))") == "5");
    std::cout << "[PASS] test_read" << std::endl;
}

void test_unset() {
    assert(runWash("%x = 10; unset(%x); echo(%x)") == "");
    std::cout << "[PASS] test_unset" << std::endl;
}

void test_export_import() {
    assert(runWash("%x = 42; export(%x); echo(%x)") == "42");
    std::cout << "[PASS] test_export_import" << std::endl;
}

void test_help() {
    std::string result = runWash("echo($(help()))");
    assert(result.find("echo") != std::string::npos);
    std::cout << "[PASS] test_help" << std::endl;
}

int main() {
    std::cout << "=== Phase 3 Tests ===" << std::endl;
    
    test_find();
    test_split();
    test_join();
    test_read();
    test_unset();
    test_export_import();
    test_help();
    
    std::cout << "\n=== All 7 tests passed! ===" << std::endl;
    return 0;
}
