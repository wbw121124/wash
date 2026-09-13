/**
 * @file test_phase2.cpp
 * @brief Phase 2 功能单元测试
 */

#include <iostream>
#include <cassert>
#include <string>
#include <cstdio>
#include <cstdlib>
#include <unistd.h>

// 执行 wash 脚本文件并返回 stdout 输出
std::string runWash(const std::string& script) {
    std::string tmpFile = "test_tmp.wash";
    FILE* f = fopen(tmpFile.c_str(), "w");
    fprintf(f, "%s\n", script.c_str());
    fflush(f);
    fclose(f);
    
    // 获取可执行文件所在目录
    char exePath[1024];
    ssize_t len = readlink("/proc/self/exe", exePath, sizeof(exePath) - 1);
    std::string cmd;
    if (len != -1) {
        exePath[len] = '\0';
        std::string exeDir = std::string(exePath);
        size_t lastSlash = exeDir.rfind('/');
        if (lastSlash != std::string::npos) {
            exeDir = exeDir.substr(0, lastSlash);
        }
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

// 检查 wash 脚本最后一行输出是否为 "1"
bool checkTrue(const std::string& script) {
    std::string result = runWash(script);
    // trim trailing whitespace and newlines
    while (!result.empty() && (result.back() == ' ' || result.back() == '\n' || result.back() == '\r')) {
        result.pop_back();
    }
    return result == "1";
}

void testPipe() {
    std::cout << "Test: pipe expression... ";
    std::string out = runWash(R"(
echo("pipe test") | %out
echo(%out)
)");
    assert(out.find("pipe test") != std::string::npos);
    std::cout << "PASS" << std::endl;
}

void testRangeStep() {
    std::cout << "Test: range with step... ";
    assert(checkTrue(R"(
%sum = 0
for(%i in 1..10..2) { %sum = calc(%sum + %i) }
echo(calc(%sum == 25))
)"));
    std::cout << "PASS" << std::endl;
}

void testStringInterpolation() {
    std::cout << "Test: string interpolation... ";
    std::string out = runWash(R"(
%name = "wash"
echo("Hello %{name}!")
)");
    assert(out.find("Hello wash!") != std::string::npos);
    std::cout << "PASS" << std::endl;
}

void testAdjacentStringConcat() {
    std::cout << "Test: adjacent string concatenation... ";
    assert(checkTrue(R"(
%full = "hello " "world"
echo(calc(%full == "hello world"))
)"));
    std::cout << "PASS" << std::endl;
}

void testCommaSeparatedRange() {
    std::cout << "Test: comma-separated range... ";
    assert(checkTrue(R"(
%result = ""
for(%i in "1..3,7..9") { %result = calc(%result + %i) }
echo(calc(%result == "123789"))
)"));
    std::cout << "PASS" << std::endl;
}

void testFunctionDef() {
    std::cout << "Test: function definition... ";
    assert(checkTrue(R"(
fadd = (a, b):{ calc(%a + %b) }
echo(calc(fadd(3, 4) == 7))
)"));
    std::cout << "PASS" << std::endl;
}

void testLambda() {
    std::cout << "Test: lambda... ";
    assert(checkTrue(R"(
double_it = (x):{ calc(%x * 2) }
echo(calc(double_it(5) == 10))
)"));
    std::cout << "PASS" << std::endl;
}

void testBuiltinFunctions() {
    std::cout << "Test: builtin functions... ";
    assert(checkTrue(R"(
echo(calc(length("hello") == 5))
)"));
    assert(checkTrue(R"(
echo(calc(upper("hello") == "HELLO"))
)"));
    assert(checkTrue(R"(
echo(calc(lower("HELLO") == "hello"))
)"));
    assert(checkTrue(R"(
echo(calc(trim("  hello  ") == "hello"))
)"));
    assert(checkTrue(R"(
echo(calc(substr("hello", 1, 3) == "ell"))
)"));
    std::cout << "PASS" << std::endl;
}

void testStringComparison() {
    std::cout << "Test: string comparison... ";
    assert(checkTrue(R"(
echo(calc("abc" == "abc"))
)"));
    assert(checkTrue(R"(
echo(calc("abc" != "def"))
)"));
    assert(checkTrue(R"(
echo(calc("abc" < "abd"))
)"));
    assert(checkTrue(R"(
echo(calc("b" > "a"))
)"));
    std::cout << "PASS" << std::endl;
}

void testNestedLoop() {
    std::cout << "Test: nested loop... ";
    assert(checkTrue(R"(
%total = 0
for(%i in 1..3) {
    for(%j in 1..3) {
        %total = calc(%total + %i * %j)
    }
}
echo(calc(%total == 36))
)"));
    std::cout << "PASS" << std::endl;
}

void testTernary() {
    std::cout << "Test: ternary expression... ";
    assert(checkTrue(R"(
%val = 10
%result = calc(%val > 5) ? "big" : "small"
echo(calc(%result == "big"))
)"));
    std::cout << "PASS" << std::endl;
}

void testBoolean() {
    std::cout << "Test: boolean... ";
    assert(checkTrue(R"(
echo(calc(true == true))
)"));
    assert(checkTrue(R"(
echo(calc(false == false))
)"));
    assert(checkTrue(R"(
echo(calc(true != false))
)"));
    std::cout << "PASS" << std::endl;
}

void testNegativeNumber() {
    std::cout << "Test: negative number... ";
    assert(checkTrue(R"(
%neg = -5
echo(calc(%neg == -5))
)"));
    assert(checkTrue(R"(
echo(calc(-3 + 5 == 2))
)"));
    std::cout << "PASS" << std::endl;
}

void testEnvVar() {
    std::cout << "Test: env variable... ";
    assert(checkTrue(R"(
%env.test_var = "hello"
%a = %env.test_var
echo(calc(%a == "hello"))
)"));
    std::cout << "PASS" << std::endl;
}

void testNestedFunctionCall() {
    std::cout << "Test: nested function call... ";
    assert(checkTrue(R"(
echo(calc(length(upper("hello")) == 5))
)"));
    std::cout << "PASS" << std::endl;
}

void testStringConcat() {
    std::cout << "Test: string concatenation... ";
    assert(checkTrue(R"(
echo(calc("hello" + " " + "world" == "hello world"))
)"));
    std::cout << "PASS" << std::endl;
}

void testConditional() {
    std::cout << "Test: conditional logic... ";
    std::string out = runWash(R"(
if(calc(1 == 1)) { echo("ok") }
)");
    assert(out.find("ok") != std::string::npos);
    std::cout << "PASS" << std::endl;
}

void testLoopControl() {
    std::cout << "Test: loop control (break)... ";
    assert(checkTrue(R"(
%count = 0
for(%i in 1..10) {
    if(calc(%i == 5)) { break }
    %count = calc(%count + 1)
}
echo(calc(%count == 4))
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

int main() {
    std::cout << "=== Phase 2 Unit Tests ===" << std::endl;
    
    testPipe();
    testRangeStep();
    testStringInterpolation();
    testAdjacentStringConcat();
    testCommaSeparatedRange();
    testFunctionDef();
    testLambda();
    testBuiltinFunctions();
    testStringComparison();
    testNestedLoop();
    testTernary();
    testBoolean();
    testNegativeNumber();
    testEnvVar();
    testNestedFunctionCall();
    testStringConcat();
    testConditional();
    testLoopControl();
    testTypeof();
    
    std::cout << "\n=== All 19 Phase 2 tests passed! ===" << std::endl;
    return 0;
}
