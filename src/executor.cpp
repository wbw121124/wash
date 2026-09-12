/**
 * @file executor.cpp
 * @brief wash 执行器实现
 * 
 * @author wash
 * @date 2026-09-12
 */

#include "executor.h"
#include <iostream>
#include <sstream>
#include <cstdlib>
#include <cmath>
#include <algorithm>
#include <cstdio>

#ifdef _WIN32
#include <windows.h>
#include <io.h>
#include <fcntl.h>
#else
#include <unistd.h>
#include <sys/wait.h>
#endif

namespace wash {

Executor::Executor() : exitCode_(0), exitRequested_(false) {
    currentScope_ = std::make_shared<Scope>();
    registerBuiltinFunctions();
}

ExecResult Executor::execute(ASTPtr program) {
    if (!program || program->type != NodeType::PROGRAM) {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(1));
    }
    
    auto programNode = std::static_pointer_cast<ProgramNode>(program);
    return executeStatements(programNode->statements);
}

void Executor::setVariable(const std::string& name, const Value& value) {
    currentScope_->set(name, value);
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
    
    // 同步到系统环境变量
    std::string envName = "wash_" + name;
    std::string envValue = valueToString(value);
    
#ifdef _WIN32
    SetEnvironmentVariableA(envName.c_str(), envValue.c_str());
#else
    setenv(envName.c_str(), envValue.c_str(), 1);
#endif
}

Value Executor::getEnvVariable(const std::string& name) {
    auto it = envVars_.find(name);
    if (it != envVars_.end()) {
        return it->second;
    }
    
    // 尝试从系统环境变量读取
    std::string envName = "wash_" + name;
#ifdef _WIN32
    char buf[1024];
    DWORD len = GetEnvironmentVariableA(envName.c_str(), buf, sizeof(buf));
    if (len > 0 && len < sizeof(buf)) {
        return makeStringValue(std::string(buf, len));
    }
#else
    const char* val = getenv(envName.c_str());
    if (val) {
        return makeStringValue(std::string(val));
    }
#endif
    
    return makeStringValue("");
}

const std::map<std::string, Value>& Executor::getAllEnvVariables() const {
    return envVars_;
}

void Executor::defineFunction(const std::string& name, Function func) {
    functions_[name] = func;
}

ExecResult Executor::callFunction(const std::string& name, const std::vector<Value>& args) {
    // 检查内建函数
    auto it = functions_.find(name);
    if (it != functions_.end()) {
        return it->second(args);
    }
    
    // 检查用户定义函数
    // TODO: 从作用域中查找用户定义的函数
    
    std::cerr << "错误: 未知函数: " << name << std::endl;
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(1));
}

void Executor::enterScope() {
    auto newScope = std::make_shared<Scope>(currentScope_);
    currentScope_ = newScope;
}

void Executor::exitScope() {
    if (currentScope_) {
        // 这里简化处理，实际应该保存父作用域
        // currentScope_ = currentScope_->parent_;
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
        case NodeType::COMMAND_EXEC:
            return executeCommand(static_cast<CommandExecNode*>(node.get()));
        case NodeType::COMMAND_OUTPUT:
            return executeCommandOutput(static_cast<CommandOutputNode*>(node.get()));
        case NodeType::IF_STMT:
            return executeIf(static_cast<IfNode*>(node.get()));
        case NodeType::FOR_STMT:
            return executeFor(static_cast<ForNode*>(node.get()));
        case NodeType::WHILE_STMT:
            return executeWhile(static_cast<WhileNode*>(node.get()));
        case NodeType::BREAK_STMT:
            return ExecResult(ExecResultType::BREAK);
        case NodeType::CONTINUE_STMT:
            return ExecResult(ExecResultType::CONTINUE);
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
    
    // 其他运算转换为数字
    double leftNum = valueToNumber(left);
    double rightNum = valueToNumber(right);
    
    if (node->op == "+") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum + rightNum));
    } else if (node->op == "-") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum - rightNum));
    } else if (node->op == "*") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum * rightNum));
    } else if (node->op == "/") {
        if (rightNum == 0) {
            std::cerr << "错误: 除零" << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        }
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum / rightNum));
    } else if (node->op == "%") {
        if (rightNum == 0) {
            std::cerr << "错误: 模零" << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        }
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::fmod(leftNum, rightNum)));
    } else if (node->op == "^") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::pow(leftNum, rightNum)));
    } else if (node->op == "&") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(static_cast<int>(leftNum) & static_cast<int>(rightNum)));
    } else if (node->op == "|") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(static_cast<int>(leftNum) | static_cast<int>(rightNum)));
    } else if (node->op == "^") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(static_cast<int>(leftNum) ^ static_cast<int>(rightNum)));
    } else if (node->op == "<<") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(static_cast<int>(leftNum) << static_cast<int>(rightNum)));
    } else if (node->op == ">>") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(static_cast<int>(leftNum) >> static_cast<int>(rightNum)));
    } else if (node->op == "<") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum < rightNum ? 1.0 : 0.0));
    } else if (node->op == "<=") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum <= rightNum ? 1.0 : 0.0));
    } else if (node->op == ">") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum > rightNum ? 1.0 : 0.0));
    } else if (node->op == ">=") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum >= rightNum ? 1.0 : 0.0));
    } else if (node->op == "==") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum == rightNum ? 1.0 : 0.0));
    } else if (node->op == "!=") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(leftNum != rightNum ? 1.0 : 0.0));
    } else if (node->op == "&&") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue((leftNum != 0) && (rightNum != 0) ? 1.0 : 0.0));
    } else if (node->op == "||") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue((leftNum != 0) || (rightNum != 0) ? 1.0 : 0.0));
    }
    
    std::cerr << "错误: 未知运算符: " << node->op << std::endl;
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeUnaryOp(UnaryOpNode* node) {
    ExecResult operandResult = executeNode(node->operand);
    if (operandResult.type != ExecResultType::NORMAL) {
        return operandResult;
    }
    
    Value operand = operandResult.value;
    double num = valueToNumber(operand);
    
    if (node->op == "-") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(-num));
    } else if (node->op == "!") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(num == 0 ? 1.0 : 0.0));
    } else if (node->op == "~") {
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(~static_cast<int>(num)));
    }
    
    std::cerr << "错误: 未知一元运算符: " << node->op << std::endl;
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeTernaryOp(TernaryOpNode* node) {
    ExecResult condResult = executeNode(node->condition);
    if (condResult.type != ExecResultType::NORMAL) {
        return condResult;
    }
    
    if (isTruthy(condResult.value)) {
        return executeNode(node->trueExpr);
    } else {
        return executeNode(node->falseExpr);
    }
}

ExecResult Executor::executeFunctionCall(FunctionCallNode* node) {
    std::vector<Value> args;
    for (const auto& arg : node->args) {
        ExecResult result = executeNode(arg);
        if (result.type != ExecResultType::NORMAL) {
            return result;
        }
        args.push_back(result.value);
    }
    
    return callFunction(node->name, args);
}

ExecResult Executor::executeAssignment(AssignmentNode* node) {
    ExecResult valueResult = executeNode(node->value);
    if (valueResult.type != ExecResultType::NORMAL) {
        return valueResult;
    }
    
    setVariable(node->varName, valueResult.value);
    return ExecResult(ExecResultType::NORMAL, valueResult.value);
}

ExecResult Executor::executeEnvAssignment(EnvAssignmentNode* node) {
    ExecResult valueResult = executeNode(node->value);
    if (valueResult.type != ExecResultType::NORMAL) {
        return valueResult;
    }
    
    setEnvVariable(node->varName, valueResult.value);
    return ExecResult(ExecResultType::NORMAL, valueResult.value);
}

ExecResult Executor::executeCommand(CommandExecNode* node) {
    std::string cmd = node->command;
    std::vector<std::string> args;
    
    for (const auto& arg : node->args) {
        ExecResult result = executeNode(arg);
        if (result.type != ExecResultType::NORMAL) {
            return result;
        }
        args.push_back(valueToString(result.value));
    }
    
    // 构建完整命令
    std::string fullCmd = cmd;
    for (const auto& arg : args) {
        fullCmd += " " + arg;
    }
    
    // 执行命令
    int exitCode = executeExternalCommand(cmd, args);
    setExitCode(exitCode);
    
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(exitCode));
}

ExecResult Executor::executeCommandOutput(CommandOutputNode* node) {
    // 执行命令并捕获输出
    std::string cmd = node->command;
    
#ifdef _WIN32
    // Windows 实现
    std::string output;
    FILE* pipe = _popen((cmd + " 2>&1").c_str(), "r");
    if (pipe) {
        char buffer[4096];
        while (fgets(buffer, sizeof(buffer), pipe)) {
            output += buffer;
        }
        _pclose(pipe);
    }
#else
    // Unix 实现
    std::string output;
    FILE* pipe = popen((cmd + " 2>&1").c_str(), "r");
    if (pipe) {
        char buffer[4096];
        while (fgets(buffer, sizeof(buffer), pipe)) {
            output += buffer;
        }
        pclose(pipe);
    }
#endif
    
    // 去掉末尾换行
    if (!output.empty() && output.back() == '\n') {
        output.pop_back();
    }
    
    return ExecResult(ExecResultType::NORMAL, makeStringValue(output));
}

ExecResult Executor::executeIf(IfNode* node) {
    ExecResult condResult = executeNode(node->condition);
    if (condResult.type != ExecResultType::NORMAL) {
        return condResult;
    }
    
    if (isTruthy(condResult.value)) {
        return executeNode(node->thenBlock);
    }
    
    for (const auto& [elifCond, elifBlock] : node->elifBlocks) {
        ExecResult elifCondResult = executeNode(elifCond);
        if (elifCondResult.type != ExecResultType::NORMAL) {
            return elifCondResult;
        }
        
        if (isTruthy(elifCondResult.value)) {
            return executeNode(elifBlock);
        }
    }
    
    if (node->elseBlock) {
        return executeNode(node->elseBlock);
    }
    
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeFor(ForNode* node) {
    std::vector<Value> range = generateRange(node->range);
    
    enterScope();
    
    for (const auto& value : range) {
        setVariable(node->varName, value);
        
        ExecResult result = executeNode(node->body);
        
        if (result.type == ExecResultType::BREAK) {
            break;
        } else if (result.type == ExecResultType::CONTINUE) {
            continue;
        } else if (result.type == ExecResultType::RETURN) {
            exitScope();
            return result;
        }
    }
    
    exitScope();
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeWhile(WhileNode* node) {
    enterScope();
    
    while (true) {
        ExecResult condResult = executeNode(node->condition);
        if (condResult.type != ExecResultType::NORMAL) {
            exitScope();
            return condResult;
        }
        
        if (!isTruthy(condResult.value)) {
            break;
        }
        
        ExecResult result = executeNode(node->body);
        
        if (result.type == ExecResultType::BREAK) {
            break;
        } else if (result.type == ExecResultType::CONTINUE) {
            continue;
        } else if (result.type == ExecResultType::RETURN) {
            exitScope();
            return result;
        }
    }
    
    exitScope();
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeReturn(ReturnNode* node) {
    if (node->value) {
        ExecResult valueResult = executeNode(node->value);
        return ExecResult(ExecResultType::RETURN, valueResult.value);
    }
    return ExecResult(ExecResultType::RETURN, makeNumberValue(0));
}

ExecResult Executor::executeFunctionDef(FunctionDefNode* node) {
    // 创建用户定义函数
    std::string name = node->name;
    ASTPtr body = node->body;
    Executor* executor = this;
    
    Function func = [executor, body](const std::vector<Value>& args) -> ExecResult {
        // TODO: 实现用户定义函数调用
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    };
    
    defineFunction(name, func);
    return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
}

ExecResult Executor::executeLambdaDef(LambdaDefNode* node) {
    // 返回一个 lambda 值
    // TODO: 实现 lambda 值
    return ExecResult(ExecResultType::NORMAL, makeStringValue("<lambda>"));
}

std::vector<Value> Executor::generateRange(ASTPtr range) {
    std::vector<Value> result;
    
    if (!range) {
        return result;
    }
    
    // 如果是字符串，解析范围
    if (range->type == NodeType::LITERAL) {
        auto literal = std::static_pointer_cast<LiteralNode>(range);
        if (std::holds_alternative<std::string>(literal->value)) {
            std::string rangeStr = std::get<std::string>(literal->value);
            // TODO: 解析范围字符串 "1..10" 或 "1,2,3"
            // 简单实现：尝试解析为数字
            try {
                double num = std::stod(rangeStr);
                result.push_back(makeNumberValue(num));
            } catch (...) {
                // 解析失败，返回空
            }
            return result;
        }
    }
    
    // 如果是二元运算（范围运算符 ..）
    if (range->type == NodeType::BINARY_OP) {
        auto binOp = std::static_pointer_cast<BinaryOpNode>(range);
        if (binOp->op == ".." || binOp->op == "..<") {
            ExecResult startResult = executeNode(binOp->left);
            ExecResult endResult = executeNode(binOp->right);
            
            if (startResult.type == ExecResultType::NORMAL && endResult.type == ExecResultType::NORMAL) {
                double start = valueToNumber(startResult.value);
                double end = valueToNumber(endResult.value);
                bool exclusive = (binOp->op == "..<");
                
                if (start <= end) {
                    for (double i = start; exclusive ? i < end : i <= end; ++i) {
                        result.push_back(makeNumberValue(i));
                    }
                } else {
                    for (double i = start; exclusive ? i > end : i >= end; --i) {
                        result.push_back(makeNumberValue(i));
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
        // 去除前后空格
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
    
#ifdef _WIN32
    int result = system(fullCmd.c_str());
#else
    pid_t pid = fork();
    
    if (pid == 0) {
        // 子进程
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
        // 父进程
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
    
    return 0;
}

int Executor::executePipe(const std::vector<std::string>& commands) {
    // TODO: 实现管道
    return 0;
}

void Executor::outputToStdout(const Value& value) {
    std::cout << valueToString(value);
}

void Executor::outputToStderr(const Value& value) {
    std::cerr << valueToString(value);
}

void Executor::registerBuiltinFunctions() {
    // echo 函数
    defineFunction("echo", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& arg : args) {
            outputToStdout(arg);
            outputToStdout(makeStringValue(" "));
        }
        outputToStdout(makeStringValue("\n"));
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    });
    
    // stderr 函数
    defineFunction("stderr", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& arg : args) {
            outputToStderr(arg);
        }
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    });
    
    // panic 函数
    defineFunction("panic", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& arg : args) {
            outputToStderr(arg);
        }
        outputToStderr(makeStringValue("\n"));
        requestExit(1);
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(1));
    });
    
    // round 函数
    defineFunction("round", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) {
            return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        }
        double num = valueToNumber(args[0]);
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::round(num)));
    });
    
    // ceil 函数
    defineFunction("ceil", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) {
            return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        }
        double num = valueToNumber(args[0]);
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::ceil(num)));
    });
    
    // floor 函数
    defineFunction("floor", [](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) {
            return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
        }
        double num = valueToNumber(args[0]);
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(std::floor(num)));
    });
    
    // argc 函数
    defineFunction("argc", [this](const std::vector<Value>& args) -> ExecResult {
        // TODO: 实现 argc
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    });
    
    // argv 函数
    defineFunction("argv", [this](const std::vector<Value>& args) -> ExecResult {
        // TODO: 实现 argv
        return ExecResult(ExecResultType::NORMAL, makeStringValue(""));
    });
    
    // return 函数（特殊处理）
    defineFunction("return", [this](const std::vector<Value>& args) -> ExecResult {
        if (args.empty()) {
            return ExecResult(ExecResultType::RETURN, makeNumberValue(0));
        }
        return ExecResult(ExecResultType::RETURN, args[0]);
    });
    
    // break 函数
    defineFunction("break", [this](const std::vector<Value>& args) -> ExecResult {
        return ExecResult(ExecResultType::BREAK);
    });
    
    // continue 函数
    defineFunction("continue", [this](const std::vector<Value>& args) -> ExecResult {
        return ExecResult(ExecResultType::CONTINUE);
    });
    
    // env 函数（查看所有环境变量）
    defineFunction("env", [this](const std::vector<Value>& args) -> ExecResult {
        for (const auto& [name, value] : envVars_) {
            outputToStdout(makeStringValue(name + "=" + valueToString(value) + "\n"));
        }
        return ExecResult(ExecResultType::NORMAL, makeNumberValue(0));
    });
}

} // namespace wash
