/**
 * @file executor.cpp
 * @brief wash 执行器实现
 * 
 * @author wash
 * @date 2026-09-12
 */

#include "executor.h"
#include "lexer.h"
#include "parser.h"
#include "color.h"
#include "compat.h"
#include <iostream>
#include <sstream>
#include <cstdlib>
#include <cmath>
#include <algorithm>
#include <cstdio>
#include <fstream>
#include <libintl.h>

#if defined(__MSYS__)
    // ========== MSYS 子系统（完整 POSIX）==========
    #include <unistd.h>
    #include <sys/wait.h>
    #define WNOHANG 1
#elif defined(__MINGW32__) || defined(__MINGW64__)
    // ========== UCRT64 / MinGW64 ==========
    #ifndef WIN32_LEAN_AND_MEAN
        #define WIN32_LEAN_AND_MEAN
    #endif
    #include <windows.h>
    #include <process.h>
    // UCRT64 的 <sys/types.h> 已通过 <cwchar> 链定义了 pid_t
    #ifndef STDIN_FILENO
        #define STDIN_FILENO 0
        #define STDOUT_FILENO 1
        #define STDERR_FILENO 2
    #endif
    #define WNOHANG 1
    #define fork() ((pid_t)-1)
    #define execvp(cmd, args) (-1)
    #define waitpid(p, s, o) 0
    #define WIFEXITED(s) 1
    #define WEXITSTATUS(s) (s)
#elif defined(_WIN32)
    // ========== 原生 Windows（MSVC）==========
    #include <windows.h>
    #include <process.h>
    #define popen _popen
    #define pclose _pclose
    #define STDIN_FILENO 0
    #define STDOUT_FILENO 1
    #define STDERR_FILENO 2
    #define execvp(cmd, args) (-1)
    #define perror(s) ((void)0)
    typedef int pid_t;
    #define fork() ((pid_t)-1)
    #define waitpid(p, s, o) 0
    #define WIFEXITED(s) 1
    #define WEXITSTATUS(s) (s)
    #define WNOHANG 1
#else
    // ========== 纯 POSIX（Linux / macOS）==========
    #include <unistd.h>
    #include <sys/wait.h>
#endif

#define _(STRING) gettext(STRING)

// POSIX environ: 声明在文件作用域，避免在 wash 命名空间内被解析为 wash::environ
#if !defined(_WIN32) || defined(__MINGW32__) || defined(__MINGW64__)
    extern char** environ;
#endif

namespace wash {

Executor::Executor() : exitCode_(0), exitRequested_(false), scriptArgc_(0) {
    currentScope_ = std::make_shared<Scope>();
    registerBuiltinFunctions();
    initSystemEnvVars();
}

void Executor::initSystemEnvVars() {
#ifdef WASH_NATIVE_WIN32
    // MSVC: 从 Windows 环境块读取
    char* envBlock = GetEnvironmentStringsA();
    if (envBlock) {
        const char* p = envBlock;
        while (*p) {
            std::string entry(p);
            size_t eq = entry.find('=');
            if (eq != std::string::npos && eq > 0) {
                std::string name = entry.substr(0, eq);
                std::string value = entry.substr(eq + 1);
                envVars_[name] = makeStringValue(value);
            }
            p += entry.size() + 1;
        }
        FreeEnvironmentStringsA(envBlock);
    }
#elif defined(__MINGW32__) || defined(__MINGW64__)
    // UCRT64/MinGW: 使用 Windows API 读取环境变量
    char* envBlock = GetEnvironmentStringsA();
    if (envBlock) {
        const char* p = envBlock;
        while (*p) {
            std::string entry(p);
            size_t eq = entry.find('=');
            if (eq != std::string::npos && eq > 0) {
                std::string name = entry.substr(0, eq);
                std::string value = entry.substr(eq + 1);
                envVars_[name] = makeStringValue(value);
            }
            p += entry.size() + 1;
        }
        FreeEnvironmentStringsA(envBlock);
    }
#else
    // POSIX: 从 environ 读取
    if (::environ) {
        for (char** env = ::environ; *env; ++env) {
            std::string entry(*env);
            size_t eq = entry.find('=');
            if (eq != std::string::npos) {
                std::string name = entry.substr(0, eq);
                std::string value = entry.substr(eq + 1);
                envVars_[name] = makeStringValue(value);
            }
        }
    }
#endif
}

void Executor::setScriptArgs(int argc, const char* const* argv) {
    scriptArgc_ = argc;
    scriptArgv_.clear();
    for (int i = 0; i < argc; ++i) {
        scriptArgv_.push_back(argv[i] ? argv[i] : "");
    }
}

ExecResult Executor::execute(ASTPtr program) {
    if (!program || program->type != NodeType::PROGRAM) {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(1));
    }
    
    auto programNode = std::static_pointer_cast<ProgramNode>(program);
    return executeStatements(programNode->statements);
}

void Executor::setVariable(const std::string& name, const Value& value) {
    currentScope_->set(name, value, true);
}

Value Executor::getVariable(const std::string& name) {
    bool found = false;
    Value val = currentScope_->get(name, found);
    if (!found) {
        return makeStringValue("");
    }
    return val;
}

void Executor::setEnvVariable(const std::string& name, const Value& value) {
    envVars_[name] = value;
    
    std::string envName = "wash_" + name;
    std::string envValue = valueToString(value);
    wash::compat::setEnvVar(envName, envValue);
}

Value Executor::getEnvVariable(const std::string& name) {
    auto it = envVars_.find(name);
    if (it != envVars_.end()) {
        return it->second;
    }
    
    // 直接查 OS 环境变量（无前缀）
    std::string val = wash::compat::getEnvVar(name);
    if (!val.empty()) {
        return makeStringValue(val);
    }
    
    // wash_ 前缀回退
    std::string washPrefixed = "wash_" + name;
    val = wash::compat::getEnvVar(washPrefixed);
    if (!val.empty()) {
        return makeStringValue(val);
    }
    
    return makeStringValue("");
}

const std::map<std::string, Value>& Executor::getAllEnvVariables() const {
    return envVars_;
}

void Executor::defineFunction(const std::string& name, Function func) {
    functions_[name] = func;
}

ExecResult Executor::callFunction(const std::string& name, const std::vector<Value>& args) {
    auto it = functions_.find(name);
    if (it != functions_.end()) {
        return it->second(args);
    }
    
    std::cerr << "错误: 未知函数: " << name << std::endl;
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(1));
}

void Executor::enterScope() {
    auto newScope = std::make_shared<Scope>(currentScope_);
    currentScope_ = newScope;
}

void Executor::exitScope() {
    auto parent = currentScope_->getParent();
    if (parent) {
        currentScope_ = parent;
    }
}

int Executor::getExitCode() const {
    return exitCode_;
}

void Executor::setExitCode(int code) {
    exitCode_ = code;
}

bool Executor::shouldExit() const {
    return exitRequested_;
}

void Executor::requestExit(int code) {
    exitRequested_ = true;
    exitCode_ = code;
}

ExecResult Executor::executeNode(ASTPtr node) {
    if (!node) {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    }
    
    switch (node->type) {
        case NodeType::LITERAL:
            return executeLiteral(static_cast<LiteralNode*>(node.get()));
        case NodeType::VARIABLE:
            return executeVariable(static_cast<VariableNode*>(node.get()));
        case NodeType::ENV_VARIABLE:
            return executeEnvVariable(static_cast<EnvVariableNode*>(node.get()));
        case NodeType::BINARY_OP:
            return executeBinaryOp(static_cast<BinaryOpNode*>(node.get()));
        case NodeType::UNARY_OP:
            return executeUnaryOp(static_cast<UnaryOpNode*>(node.get()));
        case NodeType::TERNARY_OP:
            return executeTernaryOp(static_cast<TernaryOpNode*>(node.get()));
        case NodeType::FUNCTION_CALL:
            return executeFunctionCall(static_cast<FunctionCallNode*>(node.get()));
        case NodeType::ASSIGNMENT:
            return executeAssignment(static_cast<AssignmentNode*>(node.get()));
        case NodeType::ENV_ASSIGNMENT:
            return executeEnvAssignment(static_cast<EnvAssignmentNode*>(node.get()));
        case NodeType::COMMAND_EXEC:
            return executeCommand(static_cast<CommandExecNode*>(node.get()));
        case NodeType::COMMAND_OUTPUT:
            return executeCommandOutput(static_cast<CommandOutputNode*>(node.get()));
        case NodeType::PIPE_EXPR:
            return executePipeExpr(static_cast<PipeExprNode*>(node.get()));
        case NodeType::REDIRECT_EXPR:
            return executeRedirectExpr(static_cast<RedirectExprNode*>(node.get()));
        case NodeType::IF_STMT:
            return executeIf(static_cast<IfNode*>(node.get()));
        case NodeType::FOR_STMT:
            return executeFor(static_cast<ForNode*>(node.get()));
        case NodeType::WHILE_STMT:
            return executeWhile(static_cast<WhileNode*>(node.get()));
        case NodeType::BREAK_STMT:
            return ExecResult(ExecResultType::BREAK_RES);
        case NodeType::CONTINUE_STMT:
            return ExecResult(ExecResultType::CONTINUE_RES);
        case NodeType::RETURN_STMT:
            return executeReturn(static_cast<ReturnNode*>(node.get()));
        case NodeType::FUNCTION_DEF:
            return executeFunctionDef(static_cast<FunctionDefNode*>(node.get()));
        case NodeType::LAMBDA_DEF:
            return executeLambdaDef(static_cast<LambdaDefNode*>(node.get()));
        case NodeType::PROGRAM:
        case NodeType::BLOCK: {
            auto block = std::static_pointer_cast<BlockNode>(node);
            return executeStatements(block->statements);
        }
        case NodeType::EXPRESSION_STMT: {
            auto exprStmt = std::static_pointer_cast<ExpressionStmtNode>(node);
            return executeNode(exprStmt->expression);
        }
        default:
            std::cerr << "错误: 未知节点类型" << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeNumberValue(1));
    }
}

ExecResult Executor::executeStatements(const std::vector<ASTPtr>& statements) {
    ExecResult result;
    
    for (const auto& stmt : statements) {
        result = executeNode(stmt);
        
        if (result.type != ExecResultType::NORMAL) {
            return result;
        }
        
        if (exitRequested_) {
            return result;
        }
    }
    
    return result;
}

ExecResult Executor::executeLiteral(LiteralNode* node) {
    // 如果是字符串，进行变量插值
    if (std::holds_alternative<std::string>(node->value)) {
        std::string str = std::get<std::string>(node->value);
        std::string interpolated = interpolateString(str);
        return ExecResult(ExecResultType::NORMAL, makeStringValue(interpolated));
    }
    return ExecResult(ExecResultType::NORMAL, node->value);
}

ExecResult Executor::executeVariable(VariableNode* node) {
    Value value = getVariable(node->name);
    return ExecResult(ExecResultType::NORMAL, value);
}

ExecResult Executor::executeEnvVariable(EnvVariableNode* node) {
    Value value = getEnvVariable(node->name);
    return ExecResult(ExecResultType::NORMAL, value);
}

ExecResult Executor::executeBinaryOp(BinaryOpNode* node) {
    // 逗号运算符：求值左右表达式，返回右边的值
    if (node->op == ",") {
        ExecResult leftResult = executeNode(node->left);
        ExecResult rightResult = executeNode(node->right);
        return rightResult;
    }
    
    // 范围运算符：生成序列
    if (node->op == ".." || node->op == "..<") {
        std::vector<Value> range = generateRange(std::make_shared<BinaryOpNode>(node->op, node->left, node->right));
        if (!range.empty()) {
            return ExecResult(ExecResultType::NORMAL, range[0]);
        }
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    }
    
    ExecResult leftResult = executeNode(node->left);
    if (leftResult.type != ExecResultType::NORMAL) {
        return leftResult;
    }
    
    ExecResult rightResult = executeNode(node->right);
    if (rightResult.type != ExecResultType::NORMAL) {
        return rightResult;
    }
    
    Value left = leftResult.value;
    Value right = rightResult.value;
    
    // 字符串拼接
    if (node->op == "+") {
        if (std::holds_alternative<std::string>(left) || std::holds_alternative<std::string>(right)) {
            std::string leftStr = valueToString(left);
            std::string rightStr = valueToString(right);
            return ExecResult(ExecResultType::NORMAL, makeStringValue(leftStr + rightStr));
        }
    }
    
    // 判断是否两个操作数都是整数
    bool bothInt = isInt(left) && isInt(right);
    bool eitherDouble = isDouble(left) || isDouble(right);
    
    // 整数运算
    if (bothInt) {
        int64_t l = valueToInt64(left);
        int64_t r = valueToInt64(right);
        
        if (node->op == "+") return ExecResult(ExecResultType::NORMAL, makeIntValue(l + r));
        if (node->op == "-") return ExecResult(ExecResultType::NORMAL, makeIntValue(l - r));
        if (node->op == "*") return ExecResult(ExecResultType::NORMAL, makeIntValue(l * r));
        if (node->op == "/") {
            if (r == 0) { std::cerr << "错误: 除零" << std::endl; return ExecResult(ExecResultType::NORMAL, makeIntValue(0)); }
            // 整数除法返回浮点（保持 C++ 语义）
            return ExecResult(ExecResultType::NORMAL, makeDoubleValue(static_cast<double>(l) / static_cast<double>(r)));
        }
        if (node->op == "%") {
            if (r == 0) { std::cerr << "错误: 模零" << std::endl; return ExecResult(ExecResultType::NORMAL, makeIntValue(0)); }
            return ExecResult(ExecResultType::NORMAL, makeIntValue(l % r));
        }
        if (node->op == "^") return ExecResult(ExecResultType::NORMAL, makeIntValue(l ^ r));
        if (node->op == "&") return ExecResult(ExecResultType::NORMAL, makeIntValue(l & r));
        if (node->op == "|") return ExecResult(ExecResultType::NORMAL, makeIntValue(l | r));
        if (node->op == "<<") return ExecResult(ExecResultType::NORMAL, makeIntValue(l << r));
        if (node->op == ">>") return ExecResult(ExecResultType::NORMAL, makeIntValue(l >> r));
        if (node->op == "<") return ExecResult(ExecResultType::NORMAL, makeIntValue(l < r ? 1 : 0));
        if (node->op == "<=") return ExecResult(ExecResultType::NORMAL, makeIntValue(l <= r ? 1 : 0));
        if (node->op == ">") return ExecResult(ExecResultType::NORMAL, makeIntValue(l > r ? 1 : 0));
        if (node->op == ">=") return ExecResult(ExecResultType::NORMAL, makeIntValue(l >= r ? 1 : 0));
        if (node->op == "==") return ExecResult(ExecResultType::NORMAL, makeIntValue(l == r ? 1 : 0));
        if (node->op == "!=") return ExecResult(ExecResultType::NORMAL, makeIntValue(l != r ? 1 : 0));
        if (node->op == "&&") return ExecResult(ExecResultType::NORMAL, makeIntValue((l != 0) && (r != 0) ? 1 : 0));
        if (node->op == "||") return ExecResult(ExecResultType::NORMAL, makeIntValue((l != 0) || (r != 0) ? 1 : 0));
    }
    
    // 浮点运算（至少一个操作数是 double）
    double leftNum = valueToNumber(left);
    double rightNum = valueToNumber(right);
    
    // 字符串比较（至少一个操作数是字符串）
    if (std::holds_alternative<std::string>(left) || std::holds_alternative<std::string>(right)) {
        std::string leftStr = valueToString(left);
        std::string rightStr = valueToString(right);
        if (node->op == "==") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftStr == rightStr ? 1 : 0));
        if (node->op == "!=") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftStr != rightStr ? 1 : 0));
        if (node->op == "<") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftStr < rightStr ? 1 : 0));
        if (node->op == "<=") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftStr <= rightStr ? 1 : 0));
        if (node->op == ">") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftStr > rightStr ? 1 : 0));
        if (node->op == ">=") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftStr >= rightStr ? 1 : 0));
    }
    
    if (node->op == "+") return ExecResult(ExecResultType::NORMAL, makeDoubleValue(leftNum + rightNum));
    if (node->op == "-") return ExecResult(ExecResultType::NORMAL, makeDoubleValue(leftNum - rightNum));
    if (node->op == "*") return ExecResult(ExecResultType::NORMAL, makeDoubleValue(leftNum * rightNum));
    if (node->op == "/") {
        if (rightNum == 0) { std::cerr << "错误: 除零" << std::endl; return ExecResult(ExecResultType::NORMAL, makeDoubleValue(0)); }
        return ExecResult(ExecResultType::NORMAL, makeDoubleValue(leftNum / rightNum));
    }
    if (node->op == "%") {
        if (rightNum == 0) { std::cerr << "错误: 模零" << std::endl; return ExecResult(ExecResultType::NORMAL, makeDoubleValue(0)); }
        return ExecResult(ExecResultType::NORMAL, makeDoubleValue(std::fmod(leftNum, rightNum)));
    }
    if (node->op == "^") return ExecResult(ExecResultType::NORMAL, makeIntValue(valueToInt64(left) ^ valueToInt64(right)));
    if (node->op == "&") return ExecResult(ExecResultType::NORMAL, makeIntValue(valueToInt64(left) & valueToInt64(right)));
    if (node->op == "|") return ExecResult(ExecResultType::NORMAL, makeIntValue(valueToInt64(left) | valueToInt64(right)));
    if (node->op == "<<") return ExecResult(ExecResultType::NORMAL, makeIntValue(valueToInt64(left) << valueToInt64(right)));
    if (node->op == ">>") return ExecResult(ExecResultType::NORMAL, makeIntValue(valueToInt64(left) >> valueToInt64(right)));
    if (node->op == "<") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftNum < rightNum ? 1 : 0));
    if (node->op == "<=") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftNum <= rightNum ? 1 : 0));
    if (node->op == ">") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftNum > rightNum ? 1 : 0));
    if (node->op == ">=") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftNum >= rightNum ? 1 : 0));
    if (node->op == "==") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftNum == rightNum ? 1 : 0));
    if (node->op == "!=") return ExecResult(ExecResultType::NORMAL, makeIntValue(leftNum != rightNum ? 1 : 0));
    if (node->op == "&&") return ExecResult(ExecResultType::NORMAL, makeIntValue((leftNum != 0) && (rightNum != 0) ? 1 : 0));
    if (node->op == "||") return ExecResult(ExecResultType::NORMAL, makeIntValue((leftNum != 0) || (rightNum != 0) ? 1 : 0));
    
    std::cerr << "错误: 未知运算符: " << node->op << std::endl;
    return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
}

ExecResult Executor::executeUnaryOp(UnaryOpNode* node) {
    ExecResult operandResult = executeNode(node->operand);
    if (operandResult.type != ExecResultType::NORMAL) return operandResult;
    
    if (isInt(operandResult.value)) {
        int64_t num = valueToInt64(operandResult.value);
        if (node->op == "-") return ExecResult(ExecResultType::NORMAL, makeIntValue(-num));
        if (node->op == "!") return ExecResult(ExecResultType::NORMAL, makeIntValue(num == 0 ? 1 : 0));
        if (node->op == "~") return ExecResult(ExecResultType::NORMAL, makeIntValue(~num));
    }
    
    double num = valueToNumber(operandResult.value);
    if (node->op == "-") return ExecResult(ExecResultType::NORMAL, makeDoubleValue(-num));
    if (node->op == "!") return ExecResult(ExecResultType::NORMAL, makeIntValue(num == 0 ? 1 : 0));
    if (node->op == "~") return ExecResult(ExecResultType::NORMAL, makeIntValue(~static_cast<int64_t>(num)));
    
    std::cerr << "错误: 未知一元运算符: " << node->op << std::endl;
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeTernaryOp(TernaryOpNode* node) {
    ExecResult condResult = executeNode(node->condition);
    if (condResult.type != ExecResultType::NORMAL) return condResult;
    
    if (isTruthy(condResult.value)) return executeNode(node->trueExpr);
    else return executeNode(node->falseExpr);
}

ExecResult Executor::executeFunctionCall(FunctionCallNode* node) {
    std::vector<Value> args;
    for (const auto& arg : node->args) {
        ExecResult result = executeNode(arg);
        if (result.type != ExecResultType::NORMAL) return result;
        args.push_back(result.value);
    }
    return callFunction(node->name, args);
}

ExecResult Executor::executeAssignment(AssignmentNode* node) {
    ExecResult valueResult = executeNode(node->value);
    if (valueResult.type != ExecResultType::NORMAL) return valueResult;
    setVariable(node->varName, valueResult.value);
    return ExecResult(ExecResultType::NORMAL, valueResult.value);
}

ExecResult Executor::executeEnvAssignment(EnvAssignmentNode* node) {
    ExecResult valueResult = executeNode(node->value);
    if (valueResult.type != ExecResultType::NORMAL) return valueResult;
    setEnvVariable(node->varName, valueResult.value);
    return ExecResult(ExecResultType::NORMAL, valueResult.value);
}

ExecResult Executor::executeCommand(CommandExecNode* node) {
    std::string cmd = node->command;
    std::vector<std::string> args;
    
    for (const auto& arg : node->args) {
        ExecResult result = executeNode(arg);
        if (result.type != ExecResultType::NORMAL) return result;
        args.push_back(valueToString(result.value));
    }
    
    int exitCode = executeExternalCommand(cmd, args);
    setExitCode(exitCode);
    
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(exitCode));
}

ExecResult Executor::executeCommandOutput(CommandOutputNode* node) {
    std::string cmd = node->command;
    std::string output;
    FILE* pipe = popen((cmd + " 2>&1").c_str(), "r");
    if (pipe) {
        char buffer[4096];
        while (fgets(buffer, sizeof(buffer), pipe)) {
            output += buffer;
        }
        pclose(pipe);
    }
    
    if (!output.empty() && output.back() == '\n') {
        output.pop_back();
    }
    
    return ExecResult(ExecResultType::NORMAL, makeStringValue(output));
}

std::string Executor::interpolateString(const std::string& str) {
    std::string result;
    for (size_t i = 0; i < str.size(); ++i) {
        if (str[i] == '%' && i + 1 < str.size() && str[i + 1] == '{') {
            // 找到 %{var} 模式
            i += 2; // 跳过 %{
            std::string varName;
            while (i < str.size() && str[i] != '}') {
                varName += str[i];
                i++;
            }
            // 获取变量值
            Value val = getVariable(varName);
            result += valueToString(val);
        } else {
            result += str[i];
        }
    }
    return result;
}

ExecResult Executor::executePipeExpr(PipeExprNode* node) {
    // 执行左边的命令，捕获 stdout
    ExecResult leftResult = executeNode(node->left);
    
    // 如果左边是命令执行，stdout 已经泄露到终端
    // 这里需要重新执行并捕获 stdout
    std::string output;
    if (node->left->type == NodeType::COMMAND_EXEC) {
        auto cmdNode = std::static_pointer_cast<CommandExecNode>(node->left);
        std::string cmd = cmdNode->command;
        std::vector<std::string> args;
        for (const auto& arg : cmdNode->args) {
            ExecResult r = executeNode(arg);
            if (r.type != ExecResultType::NORMAL) return r;
            args.push_back(valueToString(r.value));
        }
        
        // 构建完整命令行
        std::string fullCmd = cmd;
        for (const auto& a : args) {
            fullCmd += " " + a;
        }
        
        // 用 popen 捕获 stdout
        FILE* pipe = popen((fullCmd + " 2>&1").c_str(), "r");
        if (pipe) {
            char buffer[4096];
            while (fgets(buffer, sizeof(buffer), pipe)) {
                output += buffer;
            }
            pclose(pipe);
        }
        
        // 去掉末尾换行
        if (!output.empty() && output.back() == '\n') {
            output.pop_back();
        }
    } else if (node->left->type == NodeType::COMMAND_OUTPUT) {
        // $(...) 命令输出替换
        auto cmdNode = std::static_pointer_cast<CommandOutputNode>(node->left);
        std::string cmd = cmdNode->command;
        FILE* pipe = popen((cmd + " 2>&1").c_str(), "r");
        if (pipe) {
            char buffer[4096];
            while (fgets(buffer, sizeof(buffer), pipe)) {
                output += buffer;
            }
            pclose(pipe);
        }
        if (!output.empty() && output.back() == '\n') {
            output.pop_back();
        }
    } else {
        // 其他表达式，正常求值
        if (leftResult.type != ExecResultType::NORMAL) return leftResult;
        output = valueToString(leftResult.value);
    }
    
    // 检查右边是什么
    if (node->right->type == NodeType::VARIABLE) {
        // $cmd | %var — 捕获 stdout 赋值给变量
        auto varNode = std::static_pointer_cast<VariableNode>(node->right);
        setVariable(varNode->name, makeStringValue(output));
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    } else if (node->right->type == NodeType::COMMAND_EXEC) {
        // $cmd1 | $cmd2 — 管道：将 stdout 传给下一个命令
        auto cmdNode = std::static_pointer_cast<CommandExecNode>(node->right);
        std::string cmd = cmdNode->command;
        std::vector<std::string> args;
        for (const auto& arg : cmdNode->args) {
            ExecResult r = executeNode(arg);
            if (r.type != ExecResultType::NORMAL) return r;
            args.push_back(valueToString(r.value));
        }
        
        // 构建完整命令行，通过管道传递
        std::string fullCmd = cmd;
        for (const auto& a : args) {
            fullCmd += " " + a;
        }
        
        // 使用 echo 和管道
        std::string pipeCmd = "echo " + output + " | " + fullCmd;
        FILE* pipe = popen(pipeCmd.c_str(), "r");
        std::string pipeOutput;
        if (pipe) {
            char buffer[4096];
            while (fgets(buffer, sizeof(buffer), pipe)) {
                pipeOutput += buffer;
            }
            pclose(pipe);
        }
        if (!pipeOutput.empty() && pipeOutput.back() == '\n') {
            pipeOutput.pop_back();
        }
        return ExecResult(ExecResultType::NORMAL, makeStringValue(pipeOutput));
    }
    
    return ExecResult(ExecResultType::NORMAL, makeStringValue(output));
}

ExecResult Executor::executeRedirectExpr(RedirectExprNode* node) {
    // 解析重定向内容
    // @("in.txt", "out.txt", "err.txt") - stdin, stdout, stderr
    // @("in.txt", 1, 2) - 也可以使用 fd 编号
    
    if (!node->command) {
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    }
    
    // 保存原始 fd
    int savedStdin = wash::compat::dupFd(STDIN_FILENO);
    int savedStdout = wash::compat::dupFd(STDOUT_FILENO);
    int savedStderr = wash::compat::dupFd(STDERR_FILENO);
    
    // 解析重定向目标
    std::vector<std::string> targets;
    for (const auto& target : node->targets) {
        ExecResult r = executeNode(target);
        if (r.type != ExecResultType::NORMAL) {
            // 恢复 fd
            wash::compat::dup2Fd(savedStdin, STDIN_FILENO);
            wash::compat::dup2Fd(savedStdout, STDOUT_FILENO);
            wash::compat::dup2Fd(savedStderr, STDERR_FILENO);
            wash::compat::closeFd(savedStdin);
            wash::compat::closeFd(savedStdout);
            wash::compat::closeFd(savedStderr);
            return r;
        }
        targets.push_back(valueToString(r.value));
    }
    
    // 应用重定向
    // targets[0] -> stdin, targets[1] -> stdout, targets[2] -> stderr
    for (size_t i = 0; i < targets.size(); ++i) {
        if (targets[i].empty()) continue;
        
        int fd = -1;
        int targetFd = -1;
        
        if (i == 0) targetFd = STDIN_FILENO;
        else if (i == 1) targetFd = STDOUT_FILENO;
        else if (i == 2) targetFd = STDERR_FILENO;
        else continue;
        
        // 检查是否为 fd 编号
        try {
            int fdNum = std::stoi(targets[i]);
            if (fdNum >= 0 && fdNum <= 2) {
                // 直接 dup2
                if (fdNum != targetFd) {
                    wash::compat::dup2Fd(fdNum, targetFd);
                }
                continue;
            }
        } catch (...) {}
        
        // 打开文件
        int flags = wash::compat::FDC_O_WRONLY | wash::compat::FDC_O_CREAT | wash::compat::FDC_O_TRUNC;
        fd = wash::compat::fileOpen(targets[i], flags, 0644);
        if (fd < 0) {
            // 恢复 fd
            wash::compat::dup2Fd(savedStdin, STDIN_FILENO);
            wash::compat::dup2Fd(savedStdout, STDOUT_FILENO);
            wash::compat::dup2Fd(savedStderr, STDERR_FILENO);
            wash::compat::closeFd(savedStdin);
            wash::compat::closeFd(savedStdout);
            wash::compat::closeFd(savedStderr);
            std::cerr << "无法打开文件: " << targets[i] << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        wash::compat::dup2Fd(fd, targetFd);
        wash::compat::closeFd(fd);
    }
    
    // 执行命令
    ExecResult cmdResult = executeNode(node->command);
    
    // 恢复原始 fd
    wash::compat::dup2Fd(savedStdin, STDIN_FILENO);
    wash::compat::dup2Fd(savedStdout, STDOUT_FILENO);
    wash::compat::dup2Fd(savedStderr, STDERR_FILENO);
    wash::compat::closeFd(savedStdin);
    wash::compat::closeFd(savedStdout);
    wash::compat::closeFd(savedStderr);
    
    return cmdResult;
}

ExecResult Executor::executeIf(IfNode* node) {
    ExecResult condResult = executeNode(node->condition);
    if (condResult.type != ExecResultType::NORMAL) return condResult;
    
    if (isTruthy(condResult.value)) {
        return executeNode(node->thenBlock);
    }
    
    for (const auto& [elifCond, elifBlock] : node->elifBlocks) {
        ExecResult elifCondResult = executeNode(elifCond);
        if (elifCondResult.type != ExecResultType::NORMAL) return elifCondResult;
        if (isTruthy(elifCondResult.value)) return executeNode(elifBlock);
    }
    
    if (node->elseBlock) return executeNode(node->elseBlock);
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeFor(ForNode* node) {
    std::vector<Value> range = generateRange(node->range);
    
    enterScope();
    
    for (const auto& value : range) {
        setVariable(node->varName, value);
        ExecResult result = executeNode(node->body);
        
        if (result.type == ExecResultType::BREAK_RES) break;
        if (result.type == ExecResultType::CONTINUE_RES) continue;
        if (result.type == ExecResultType::RETURN_RES) { exitScope(); return result; }
    }
    
    exitScope();
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeWhile(WhileNode* node) {
    enterScope();
    
    while (true) {
        ExecResult condResult = executeNode(node->condition);
        if (condResult.type != ExecResultType::NORMAL) { exitScope(); return condResult; }
        if (!isTruthy(condResult.value)) break;
        
        ExecResult result = executeNode(node->body);
        if (result.type == ExecResultType::BREAK_RES) break;
        if (result.type == ExecResultType::CONTINUE_RES) continue;
        if (result.type == ExecResultType::RETURN_RES) { exitScope(); return result; }
    }
    
    exitScope();
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeReturn(ReturnNode* node) {
    if (node->value) {
        ExecResult valueResult = executeNode(node->value);
        return ExecResult(ExecResultType::RETURN_RES, valueResult.value);
    }
    return ExecResult(ExecResultType::RETURN_RES, makeNumberValue(0));
}

ExecResult Executor::executeFunctionDef(FunctionDefNode* node) {
    std::string name = node->name;
    ASTPtr body = node->body;
    std::vector<std::string> paramNames = node->paramNames;
    Executor* self = this;
    
    Function func = [self, body, paramNames](const std::vector<Value>& args) -> ExecResult {
        self->enterScope();
        
        // 绑定参数到命名参数或位置参数
        for (size_t i = 0; i < args.size(); ++i) {
            if (i < paramNames.size()) {
                // 使用命名参数
                self->setVariable(paramNames[i], args[i]);
            }
            // 也绑定到 %0, %1, %2... 位置参数
            self->setVariable("$" + std::to_string(i), args[i]);
        }
        
        ExecResult result = self->executeNode(body);
        self->exitScope();
        
        if (result.type == ExecResultType::RETURN_RES) {
            return ExecResult(ExecResultType::NORMAL, result.value);
        }
        return result;
    };
    
    defineFunction(name, func);
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeLambdaDef(LambdaDefNode* node) {
    ASTPtr body = node->body;
    std::vector<std::string> paramNames = node->paramNames;
    Executor* self = this;
    
    Function func = [self, body, paramNames](const std::vector<Value>& args) -> ExecResult {
        self->enterScope();
        
        // 绑定参数到命名参数或位置参数
        for (size_t i = 0; i < args.size(); ++i) {
            if (i < paramNames.size()) {
                // 使用命名参数
                self->setVariable(paramNames[i], args[i]);
            }
            // 也绑定到 %0, %1, %2... 位置参数
            self->setVariable("$" + std::to_string(i), args[i]);
        }
        
        ExecResult result = self->executeNode(body);
        self->exitScope();
        
        if (result.type == ExecResultType::RETURN_RES) {
            return ExecResult(ExecResultType::NORMAL, result.value);
        }
        return result;
    };
    
    // 存储 lambda 到临时函数表，返回函数引用
    static int lambdaCounter = 0;
    std::string lambdaName = "__lambda_" + std::to_string(lambdaCounter++);
    defineFunction(lambdaName, func);
    
    // 返回一个包含函数名的特殊值，供后续调用
    return ExecResult(ExecResultType::NORMAL, makeStringValue(lambdaName));
}

std::vector<Value> Executor::generateRange(ASTPtr range) {
    std::vector<Value> result;
    if (!range) return result;
    
    if (range->type == NodeType::LITERAL) {
        auto literal = std::static_pointer_cast<LiteralNode>(range);
        if (std::holds_alternative<std::string>(literal->value)) {
            std::string rangeStr = std::get<std::string>(literal->value);
            // 支持逗号分隔的范围："1..10,20..40,0"
            std::istringstream iss(rangeStr);
            std::string part;
            while (std::getline(iss, part, ',')) {
                // 去除空格
                size_t start = part.find_first_not_of(" \t");
                size_t end = part.find_last_not_of(" \t");
                if (start == std::string::npos) continue;
                std::string trimmed = part.substr(start, end - start + 1);
                
                // 检查是否为范围 a..b
                size_t dotdot = trimmed.find("..");
                if (dotdot != std::string::npos) {
                    std::string s = trimmed.substr(0, dotdot);
                    std::string e = trimmed.substr(dotdot + 2);
                    bool excl = false;
                    if (!e.empty() && e[0] == '<') {
                        excl = true;
                        e = e.substr(1);
                    }
                    try {
                        double sv = std::stod(s);
                        double ev = std::stod(e);
                        if (sv <= ev) {
                            for (double i = sv; excl ? i < ev : i <= ev; ++i) {
                                result.push_back(makeIntValue(static_cast<int64_t>(i)));
                            }
                        } else {
                            for (double i = sv; excl ? i > ev : i >= ev; --i) {
                                result.push_back(makeIntValue(static_cast<int64_t>(i)));
                            }
                        }
                    } catch (...) {}
                } else {
                    // 单个值
                    try {
                        double num = std::stod(trimmed);
                        result.push_back(makeIntValue(static_cast<int64_t>(num)));
                    } catch (...) {}
                }
            }
            return result;
        }
    }
    
    if (range->type == NodeType::BINARY_OP) {
        auto binOp = std::static_pointer_cast<BinaryOpNode>(range);
        
        // a..b..s 步长语法
        if (binOp->op == "..step") {
            // binOp->left 是 BinaryOpNode(op="..", left=start, right=end)
            // binOp->right 是 step
            auto innerOp = std::static_pointer_cast<BinaryOpNode>(binOp->left);
            ExecResult startResult = executeNode(innerOp->left);
            ExecResult endResult = executeNode(innerOp->right);
            ExecResult stepResult = executeNode(binOp->right);
            
            if (startResult.type == ExecResultType::NORMAL && 
                endResult.type == ExecResultType::NORMAL &&
                stepResult.type == ExecResultType::NORMAL) {
                double start = valueToNumber(startResult.value);
                double end = valueToNumber(endResult.value);
                double step = valueToNumber(stepResult.value);
                
                if (step == 0) {
                    std::cerr << "错误: 范围步长不能为 0" << std::endl;
                    return result;
                }
                if (step > 0 && start > end) {
                    std::cerr << "错误: 步长为正时起始值不能大于结束值" << std::endl;
                    return result;
                }
                if (step < 0 && start < end) {
                    std::cerr << "错误: 步长为负时起始值不能小于结束值" << std::endl;
                    return result;
                }
                
                if (step > 0) {
                    for (double i = start; i <= end; i += step) {
                        result.push_back(makeIntValue(static_cast<int64_t>(i)));
                    }
                } else {
                    for (double i = start; i >= end; i += step) {
                        result.push_back(makeIntValue(static_cast<int64_t>(i)));
                    }
                }
            }
        } else if (binOp->op == ".." || binOp->op == "..<") {
            ExecResult startResult = executeNode(binOp->left);
            ExecResult endResult = executeNode(binOp->right);
            
            if (startResult.type == ExecResultType::NORMAL && endResult.type == ExecResultType::NORMAL) {
                double start = valueToNumber(startResult.value);
                double end = valueToNumber(endResult.value);
                bool exclusive = (binOp->op == "..<");
                
                if (start <= end) {
                    for (double i = start; exclusive ? i < end : i <= end; ++i) {
                        result.push_back(makeIntValue(static_cast<int64_t>(i)));
                    }
                } else {
                    for (double i = start; exclusive ? i > end : i >= end; --i) {
                        result.push_back(makeIntValue(static_cast<int64_t>(i)));
                    }
                }
            }
        }
    }
    
    return result;
}

std::vector<std::string> Executor::parseRedirectContent(const std::string& content) {
    std::vector<std::string> result;
    std::istringstream iss(content);
    std::string token;
    
    while (std::getline(iss, token, ',')) {
        size_t start = token.find_first_not_of(" \t");
        size_t end = token.find_last_not_of(" \t");
        if (start != std::string::npos) {
            result.push_back(token.substr(start, end - start + 1));
        }
    }
    
    return result;
}

int Executor::executeExternalCommand(const std::string& cmd, const std::vector<std::string>& args) {
    std::string fullCmd = cmd;
    for (const auto& arg : args) {
        fullCmd += " " + arg;
    }
    
#if defined(__MINGW32__) || defined(__MINGW64__)
    // UCRT64/MinGW: 使用 system() 替代 fork/execvp
    // system() 会通过 cmd.exe 执行，继承父进程的 stdout/stderr
    int ret = system(fullCmd.c_str());
    if (ret == -1) {
        return -1;
    }
    return ret;
#else
    pid_t pid = fork();
    
    if (pid == 0) {
        std::vector<char*> cArgs;
        cArgs.push_back(const_cast<char*>(cmd.c_str()));
        for (const auto& arg : args) {
            cArgs.push_back(const_cast<char*>(arg.c_str()));
        }
        cArgs.push_back(nullptr);
        
        execvp(cmd.c_str(), cArgs.data());
        perror("execvp");
        exit(1);
    } else if (pid > 0) {
        int status;
        waitpid(pid, &status, 0);
        
        if (WIFEXITED(status)) {
            return WEXITSTATUS(status);
        }
        return -1;
    } else {
        perror("fork");
        return -1;
    }
#endif
}

void Executor::outputToStdout(const Value& value) {
    std::cout << valueToString(value);
}

void Executor::outputToStderr(const Value& value) {
    std::cerr << valueToString(value);
}

void Executor::registerBuiltinFunctions() {
    defineFunction("echo", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& arg : args) {
            outputToStdout(arg);
            outputToStdout(makeStringValue(" "));
        }
        outputToStdout(makeStringValue("\n"));
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    });
    
    defineFunction("stderr", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& arg : args) {
            outputToStderr(arg);
        }
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    });
    
    defineFunction("panic", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& arg : args) {
            outputToStderr(arg);
        }
        outputToStderr(makeStringValue("\n"));
        requestExit(1);
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(1));
    });
    
    defineFunction("round", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::round(valueToNumber(args[0]))));
    });
    
    defineFunction("ceil", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::ceil(valueToNumber(args[0]))));
    });
    
    defineFunction("floor", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::floor(valueToNumber(args[0]))));
    });
    
    defineFunction("argc", [this](const std::vector<Value>& args) -> ExecResult {
        // argc() 返回脚本参数个数（不包括脚本名），即 scriptArgc_ - 1
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(scriptArgc_ > 0 ? scriptArgc_ - 1 : 0));
    });
    
    defineFunction("argv", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        int index = static_cast<int>(valueToNumber(args[0]));
        if (index < 0 || index >= static_cast<int>(scriptArgv_.size())) {
            return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        }
        return ExecResult(ExecResultType::NORMAL, makeStringValue(scriptArgv_[index]));
    });
    
    defineFunction("return", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::RETURN_RES, makeNumberValue(0));
        return ExecResult(ExecResultType::RETURN_RES, args[0]);
    });
    
    defineFunction("length", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        std::string str = valueToString(args[0]);
        return ExecResult(ExecResultType::NORMAL, makeIntValue(static_cast<int64_t>(str.size())));
    });
    
    defineFunction("upper", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        std::string str = valueToString(args[0]);
        for (auto& c : str) c = std::toupper(c);
        return ExecResult(ExecResultType::NORMAL, makeStringValue(str));
    });
    
    defineFunction("lower", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        std::string str = valueToString(args[0]);
        for (auto& c : str) c = std::tolower(c);
        return ExecResult(ExecResultType::NORMAL, makeStringValue(str));
    });
    
    defineFunction("trim", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        std::string str = valueToString(args[0]);
        size_t start = str.find_first_not_of(" \t\n\r");
        size_t end = str.find_last_not_of(" \t\n\r");
        if (start == std::string::npos) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        return ExecResult(ExecResultType::NORMAL, makeStringValue(str.substr(start, end - start + 1)));
    });
    
    defineFunction("substr", [](const std::vector<Value>& args) -> ExecResult {
        if (args.size() < 2) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        std::string str = valueToString(args[0]);
        size_t start = static_cast<size_t>(valueToInt64(args[1]));
        size_t len = (args.size() >= 3) ? static_cast<size_t>(valueToInt64(args[2])) : std::string::npos;
        if (start >= str.size()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        return ExecResult(ExecResultType::NORMAL, makeStringValue(str.substr(start, len)));
    });
    
    defineFunction("typeof", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue("undefined"));
        const Value& v = args[0];
        if (isInt(v)) return ExecResult(ExecResultType::NORMAL, makeStringValue("int"));
        if (isDouble(v)) return ExecResult(ExecResultType::NORMAL, makeStringValue("double"));
        return ExecResult(ExecResultType::NORMAL, makeStringValue("string"));
    });
    
    defineFunction("find", [](const std::vector<Value>& args) -> ExecResult {
        if (args.size() < 2) return ExecResult(ExecResultType::NORMAL, makeIntValue(-1));
        std::string str = valueToString(args[0]);
        std::string sub = valueToString(args[1]);
        size_t pos = str.find(sub);
        if (pos == std::string::npos) return ExecResult(ExecResultType::NORMAL, makeIntValue(-1));
        return ExecResult(ExecResultType::NORMAL, makeIntValue(static_cast<int64_t>(pos)));
    });
    
    defineFunction("split", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        std::string str = valueToString(args[0]);
        std::string delim = (args.size() >= 2) ? valueToString(args[1]) : " ";
        
        // 返回一个逗号分隔的字符串，可以用 for..in 遍历
        std::string result;
        size_t start = 0;
        size_t end;
        bool first = true;
        while ((end = str.find(delim, start)) != std::string::npos) {
            if (!first) result += ",";
            result += str.substr(start, end - start);
            start = end + delim.size();
            first = false;
        }
        if (!first) result += ",";
        result += str.substr(start);
        
        return ExecResult(ExecResultType::NORMAL, makeStringValue(result));
    });
    
    defineFunction("join", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        std::string rangeStr = valueToString(args[0]);
        std::string delim = (args.size() >= 2) ? valueToString(args[1]) : " ";
        
        // 简单实现：直接用分隔符替换逗号
        std::string result;
        for (size_t i = 0; i < rangeStr.size(); ++i) {
            if (rangeStr[i] == ',') {
                result += delim;
            } else {
                result += rangeStr[i];
            }
        }
        return ExecResult(ExecResultType::NORMAL, makeStringValue(result));
    });
    
    defineFunction("read", [this](const std::vector<Value>& args) -> ExecResult {
        std::string prompt;
        if (!args.empty()) {
            prompt = valueToString(args[0]);
        }
        
        if (!prompt.empty()) {
            std::cout << prompt << std::flush;
        }
        
        std::string line;
        std::getline(std::cin, line);
        return ExecResult(ExecResultType::NORMAL, makeStringValue(line));
    });
    
    defineFunction("source", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        std::string filename = valueToString(args[0]);
        
        // 读取文件内容
        std::ifstream file(filename);
        if (!file.is_open()) {
            std::cerr << "错误: 无法打开文件: " << filename << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        std::string content((std::istreambuf_iterator<char>(file)),
                            std::istreambuf_iterator<char>());
        file.close();
        
        // 词法分析
        Lexer lexer(content, filename);
        auto tokens = lexer.tokenize();
        
        if (lexer.hasError()) {
            std::cerr << "错误: 词法分析失败" << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        // 语法分析
        Parser parser(tokens, filename);
        auto ast = parser.parse();
        
        if (parser.hasError()) {
            std::cerr << parser.getError() << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        // 执行
        return executeNode(ast);
    });
    
    defineFunction("unset", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        std::string name = valueToString(args[0]);
        // 简单实现：设置为空字符串
        setVariable(name, makeStringValue(""));
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    });
    
    defineFunction("export", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        std::string name = valueToString(args[0]);
        Value val = getVariable(name);
        setEnvVariable(name, val);
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    });
    
    defineFunction("help", [this](const std::vector<Value>& args) -> ExecResult {
        wash::ColorManager cm;
        std::string bold = "\033[1m";
        std::string cyan = cm.fgStr(wash::Color::CYAN);
        std::string green = cm.fgStr(wash::Color::GREEN);
        std::string yellow = cm.fgStr(wash::Color::YELLOW);
        std::string dim = "\033[2m";
        std::string rst = cm.resetStr();
        
        // 单命令详细帮助
        if (!args.empty()) {
            std::string cmd = valueToString(args[0]);
            
            static const std::map<std::string, std::string> helpDB = {
                {"echo",     _("help_echo")},
                {"stderr",   _("help_stderr")},
                {"panic",    _("help_panic")},
                {"round",    _("help_round")},
                {"ceil",     _("help_ceil")},
                {"floor",    _("help_floor")},
                {"length",   _("help_length")},
                {"upper",    _("help_upper")},
                {"lower",    _("help_lower")},
                {"trim",     _("help_trim")},
                {"substr",   _("help_substr")},
                {"find",     _("help_find")},
                {"split",    _("help_split")},
                {"join",     _("help_join")},
                {"read",     _("help_read")},
                {"source",   _("help_source")},
                {"unset",    _("help_unset")},
                {"export",   _("help_export")},
                {"typeof",   _("help_typeof")},
                {"help",     _("help_help")},
                {"argc",     _("help_argc")},
                {"argv",     _("help_argv")},
                {"return",   _("help_return")},
                {"break",    _("help_break")},
                {"continue", _("help_continue")},
                {"fg",       _("help_fg")},
                {"bg",       _("help_bg")},
                {"colors",   _("help_colors")},
            };
            
            auto it = helpDB.find(cmd);
            if (it != helpDB.end()) {
                outputToStdout(makeStringValue(bold + cyan + cmd + rst + "\n" + dim + "------" + rst + "\n" + green + it->second + rst + "\n"));
            } else {
                outputToStdout(makeStringValue(_("help_unknown") + cmd + "\n"));
            }
            return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
        }
        
        // 完整帮助列表
        struct HelpEntry { const char* name; const char* brief; };
        static const HelpEntry entries[] = {
            {"echo",     _("help_brief_echo")},
            {"stderr",   _("help_brief_stderr")},
            {"panic",    _("help_brief_panic")},
            {"round",    _("help_brief_round")},
            {"ceil",     _("help_brief_ceil")},
            {"floor",    _("help_brief_floor")},
            {"length",   _("help_brief_length")},
            {"upper",    _("help_brief_upper")},
            {"lower",    _("help_brief_lower")},
            {"trim",     _("help_brief_trim")},
            {"substr",   _("help_brief_substr")},
            {"find",     _("help_brief_find")},
            {"split",    _("help_brief_split")},
            {"join",     _("help_brief_join")},
            {"read",     _("help_brief_read")},
            {"source",   _("help_brief_source")},
            {"unset",    _("help_brief_unset")},
            {"export",   _("help_brief_export")},
            {"typeof",   _("help_brief_typeof")},
            {"help",     _("help_brief_help")},
            {"argc",     _("help_brief_argc")},
            {"argv",     _("help_brief_argv")},
            {"return",   _("help_brief_return")},
            {"break",    _("help_brief_break")},
            {"continue", _("help_brief_continue")},
            {"fg",       _("help_brief_fg")},
            {"bg",       _("help_brief_bg")},
            {"colors",   _("help_brief_colors")},
        };
        
        outputToStdout(makeStringValue(bold + _("wash_builtins_title") + rst + "\n"));
        outputToStdout(makeStringValue(dim + _("wash_builtins_hint") + rst + "\n\n"));
        for (const auto& e : entries) {
            outputToStdout(makeStringValue("  " + cyan + std::string(e.name) + rst + "  " + dim + std::string(e.brief) + rst + "\n"));
        }
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    });
    
    defineFunction("keys", [this](const std::vector<Value>& args) -> ExecResult {
        std::string result;
        std::shared_ptr<Scope> scope = currentScope_;
        bool first = true;
        while (scope) {
            auto names = scope->getVariableNames();
            for (const auto& name : names) {
                if (!first) result += ",";
                result += name;
                first = false;
            }
            scope = scope->getParent();
        }
        return ExecResult(ExecResultType::NORMAL, makeStringValue(result));
    });
    
    defineFunction("break", [](const std::vector<Value>& args) -> ExecResult {
        return ExecResult(ExecResultType::BREAK_RES);
    });
    
    defineFunction("continue", [](const std::vector<Value>& args) -> ExecResult {
        return ExecResult(ExecResultType::CONTINUE_RES);
    });
    
    defineFunction("env", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& [name, value] : envVars_) {
            outputToStdout(makeStringValue(name + "=" + valueToString(value) + "\n"));
        }
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    });
    
    defineFunction("colors", [this](const std::vector<Value>& args) -> ExecResult {
        wash::ColorManager cm;
        if (cm.hasColor()) {
            outputToStdout("终端支持颜色\n");
            // 显示深度信息
            switch (cm.getDepth()) {
                case wash::ColorDepth::TRUECOLOR:
                    outputToStdout("颜色深度: 真彩色 (24-bit)\n"); break;
                case wash::ColorDepth::COLOR_256:
                    outputToStdout("颜色深度: 256 色\n"); break;
                case wash::ColorDepth::COLOR_16:
                    outputToStdout("颜色深度: 16 色\n"); break;
                case wash::ColorDepth::COLOR_8:
                    outputToStdout("颜色深度: 8 色\n"); break;
                default:
                    outputToStdout("颜色深度: 未知\n"); break;
            }
            outputToStdout("可用颜色:\n");
            outputToStdout(cm.fgStr(wash::Color::BLACK) + "  黑色" + cm.resetStr() + "\n");
            outputToStdout(cm.fgStr(wash::Color::RED) + "  红色" + cm.resetStr() + "\n");
            outputToStdout(cm.fgStr(wash::Color::GREEN) + "  绿色" + cm.resetStr() + "\n");
            outputToStdout(cm.fgStr(wash::Color::YELLOW) + "  黄色" + cm.resetStr() + "\n");
            outputToStdout(cm.fgStr(wash::Color::BLUE) + "  蓝色" + cm.resetStr() + "\n");
            outputToStdout(cm.fgStr(wash::Color::MAGENTA) + "  品红" + cm.resetStr() + "\n");
            outputToStdout(cm.fgStr(wash::Color::CYAN) + "  青色" + cm.resetStr() + "\n");
            outputToStdout(cm.fgStr(wash::Color::WHITE) + "  白色" + cm.resetStr() + "\n");
            if (cm.getDepth() == wash::ColorDepth::COLOR_256 ||
                cm.getDepth() == wash::ColorDepth::TRUECOLOR) {
                outputToStdout("\n256 色示例:\n");
                for (int i = 0; i < 12; ++i) {
                    outputToStdout(cm.fgStr(i) + " " + std::to_string(i) + cm.resetStr() + " ");
                }
                outputToStdout("\n");
            }
        } else {
            outputToStdout("终端不支持颜色\n");
        }
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    });
    
    defineFunction("fg", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) {
            outputToStderr("用法: fg(color)\n");
            outputToStderr("  颜色名称: red, green, blue, ...\n");
            outputToStderr("  256色索引: 0-255\n");
            outputToStderr("  真彩色: \"R,G,B\"\n");
            outputToStderr("  重置: \"reset\"\n");
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        wash::ColorManager cm;
        std::string seq;
        
        if (isInt(args[0])) {
            seq = cm.fgStr(static_cast<int>(valueToInt64(args[0])));
        } else if (isDouble(args[0])) {
            seq = cm.fgStr(static_cast<int>(valueToNumber(args[0])));
        } else {
            seq = cm.fgStr(valueToString(args[0]));
        }
        
        if (!seq.empty()) {
            outputToStdout(seq);
            return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
        }
        
        outputToStderr("错误: 无法识别的颜色参数\n");
        return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
    });
    
    defineFunction("bg", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) {
            outputToStderr("用法: bg(color)\n");
            outputToStderr("  颜色名称: red, green, blue, ...\n");
            outputToStderr("  256色索引: 0-255\n");
            outputToStderr("  真彩色: \"R,G,B\"\n");
            outputToStderr("  重置: \"reset\"\n");
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        wash::ColorManager cm;
        std::string seq;
        
        if (isInt(args[0])) {
            seq = cm.bgStr(static_cast<int>(valueToInt64(args[0])));
        } else if (isDouble(args[0])) {
            seq = cm.bgStr(static_cast<int>(valueToNumber(args[0])));
        } else {
            seq = cm.bgStr(valueToString(args[0]));
        }
        
        if (!seq.empty()) {
            outputToStdout(seq);
            return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
        }
        
        outputToStderr("错误: 无法识别的颜色参数\n");
        return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
    });
    
    // 模块系统：run() - 子环境执行
    defineFunction("run", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        
        std::string filename = valueToString(args[0]);
        
        // 传递参数给子进程
        std::vector<std::string> cmdArgs;
        for (size_t i = 1; i < args.size(); ++i) {
            cmdArgs.push_back(valueToString(args[i]));
        }
        
        // 读取并执行文件
        std::ifstream file(filename);
        if (!file.is_open()) {
            outputToStderr("错误: 无法打开文件: " + filename + "\n");
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        std::string source((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
        file.close();
        
        // 创建子作用域执行
        enterScope();
        setScriptArgs(args.size(), nullptr);
        auto result = executeSource(source, filename);
        exitScope();
        
        return ExecResult(ExecResultType::NORMAL, makeIntValue(result.exitCode));
    });
    
    // 模块系统：include() - 当前环境执行
    defineFunction("include", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        
        std::string filename = valueToString(args[0]);
        
        // 读取并执行文件
        std::ifstream file(filename);
        if (!file.is_open()) {
            outputToStderr("错误: 无法打开文件: " + filename + "\n");
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        std::string source((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
        file.close();
        
        // 在当前环境执行
        auto result = executeSource(source, filename);
        return ExecResult(ExecResultType::NORMAL, makeIntValue(result.exitCode));
    });
    
    // 模块系统：export() - 导出函数/变量
    defineFunction("export", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
        
        std::string name = valueToString(args[0]);
        
        // 获取变量值
        Value val = getVariable(name);
        if (std::holds_alternative<std::string>(val) && std::get<std::string>(val).empty()) {
            // 变量不存在，检查是否为函数
            // 函数导出：标记为可导出
            exportTable_[name] = ExportItem::FUNCTION;
        } else {
            // 变量导出
            exportTable_[name] = ExportItem::VARIABLE;
            exportValues_[name] = val;
        }
        
        return ExecResult(ExecResultType::NORMAL, makeIntValue(0));
    });
    
    // 模块系统：import() - 模块导入与映射
    defineFunction("import", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        
        std::string filename = valueToString(args[0]);
        
        // 读取文件
        std::ifstream file(filename);
        if (!file.is_open()) {
            outputToStderr("错误: 无法打开文件: " + filename + "\n");
            return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
        }
        
        std::string source((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
        file.close();
        
        // 创建临时作用域执行
        enterScope();
        auto result = executeSource(source, filename);
        
        // 收集导出的项
        std::map<std::string, Value> exports;
        if (!exportTable_.empty()) {
            for (const auto& [name, type] : exportTable_) {
                if (type == ExportItem::VARIABLE) {
                    exports[name] = exportValues_[name];
                }
            }
        }
        exitScope();
        
        // 返回第一个参数（如果有映射）
        if (args.size() > 1) {
            return ExecResult(ExecResultType::NORMAL, args[1]);
        }
        
        return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
    });
}

Executor::ExecResult2 Executor::executeSource(const std::string& source, const std::string& filename) {
    wash::Lexer lexer(source);
    auto tokens = lexer.tokenize();
    if (lexer.hasError()) {
        outputToStderr("词法错误\n");
        return ExecResult2{1};
    }
    
    wash::Parser parser(tokens, filename);
    auto ast = parser.parse();
    if (parser.hasError()) {
        outputToStderr(parser.getError() + "\n");
        return ExecResult2{1};
    }
    
    auto result = execute(ast);
    return ExecResult2{getExitCode()};
}

} // namespace wash
