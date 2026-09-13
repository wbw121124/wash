/**
 * @file test_phase3.cpp
 * @brief Phase 3 内建函数单元测试
 */

#include <iostream>
#include <cassert>
#include <string>
#include <cstdio>
#include <cstdlib>
#include <unistd.h>

std::string runWash(const std::string& script) {
    std::string tmpFile = "test_tmp.wash";
    FILE* f = fopen(tmpFile.c_str(), "w");
    fprintf(f, "%s\n", script.c_str());
    fflush(f);
    fclose(f);
    
    char exePath[1024];
    ssize_t len = readlink("/proc/self/exe", exePath, sizeof(exePath) - 1);
    std::string cmd;
    if (len != -1) {
        exePath[len] = '\0';
        std::string exeDir = std::string(exePath);
        size_t lastSlash = exeDir.rfind('/');
        if (lastSlash != std::string::npos) exeDir = exeDir.substr(0, lastSlash);
        cmd = exeDir + "/wash.exe " + tmpFile;
    } else {
        cmd = "./wash.exe " + tmpFile;
    }
    
    FILE* pipe = popen(cmd.c_str(), "r");
    std::string output;
    char buf[4096];
    while (fgets(buf, sizeof(buf), pipe)) output += buf;
    pclose(pipe);
    remove(tmpFile.c_str());
    
    while (!output.empty() && (output.back() == '\n' || output.back() == '\r')) output.pop_back();
    return output;
}

bool checkTrue(const std::string& script) {
    std::string result = runWash(script);
    while (!result.empty() && result.back() == ' ') result.pop_back();
    return result == "1";
}

void testFind() {
    std::cout << "Test: find... ";
    assert(checkTrue(R"(
echo(calc(find("hello world", "world") == 6))
)"));
    assert(checkTrue(R"(
echo(calc(find("hello", "xyz") == -1))
)"));
    std::cout << "PASS" << std::endl;
}

void testTypeof() {
    std::cout << "Test: typeof... ";
    assert(checkTrue(R"(
echo(calc(typeof(42) == "int"))
)"));
    assert(checkTrue(R"(
echo(calc(typeof("hello") == "string"))
)"));
    std::cout << "PASS" << std::endl;
}

void testLength() {
    std::cout << "Test: length... ";
    assert(checkTrue(R"(
echo(calc(length("hello") == 5))
)"));
    std::cout << "PASS" << std::endl;
}

void testUpperLower() {
    std::cout << "Test: upper/lower... ";
    assert(checkTrue(R"(
echo(calc(upper("hello") == "HELLO"))
)"));
    assert(checkTrue(R"(
echo(calc(lower("HELLO") == "hello"))
)"));
    std::cout << "PASS" << std::endl;
}

void testTrim() {
    std::cout << "Test: trim... ";
    assert(checkTrue(R"(
echo(calc(trim("  hello  ") == "hello"))
)"));
    std::cout << "PASS" << std::endl;
}

void testSubstr() {
    std::cout << "Test: substr... ";
    assert(checkTrue(R"(
echo(calc(substr("hello", 1, 3) == "ell"))
)"));
    std::cout << "PASS" << std::endl;
}

void testHelp() {
    std::cout << "Test: help... ";
    std::string out = runWash("help()");
    assert(out.find("wash") != std::string::npos);
    assert(out.find("echo") != std::string::npos);
    std::cout << "PASS" << std::endl;
}

int main() {
    std::cout << "=== Phase 3 Unit Tests ===" << std::endl;
    
    testFind();
    testTypeof();
    testLength();
    testUpperLower();
    testTrim();
    testSubstr();
    testHelp();
    
    std::cout << "\n=== All 7 Phase 3 tests passed! ===" << std::endl;
    return 0;
}
