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
#include <unistd.h>
#include <sys/wait.h>
#include <fcntl.h>

namespace wash {

Executor::Executor() : exitCode_(0), exitRequested_(false), scriptArgc_(0) {
    currentScope_ = std::make_shared<Scope>();
    registerBuiltinFunctions();
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
    setenv(envName.c_str(), envValue.c_str(), 1);
}

Value Executor::getEnvVariable(const std::string& name) {
    auto it = envVars_.find(name);
    if (it != envVars_.end()) {
        return it->second;
    }
    
    std::string envName = "wash_" + name;
    const char* val = getenv(envName.c_str());
    if (val) {
        return makeStringValue(std::string(val));
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
    int savedStdin = dup(STDIN_FILENO);
    int savedStdout = dup(STDOUT_FILENO);
    int savedStderr = dup(STDERR_FILENO);
    
    // 解析重定向目标
    std::vector<std::string> targets;
    for (const auto& target : node->targets) {
        ExecResult r = executeNode(target);
        if (r.type != ExecResultType::NORMAL) {
            // 恢复 fd
            dup2(savedStdin, STDIN_FILENO);
            dup2(savedStdout, STDOUT_FILENO);
            dup2(savedStderr, STDERR_FILENO);
            close(savedStdin);
            close(savedStdout);
            close(savedStderr);
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
                    dup2(fdNum, targetFd);
                }
                continue;
            }
        } catch (...) {}
        
        // 打开文件
        const char* mode = (targetFd == STDIN_FILENO) ? "r" : "w";
        fd = open(targets[i].c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) {
            // 恢复 fd
            dup2(savedStdin, STDIN_FILENO);
            dup2(savedStdout, STDOUT_FILENO);
            dup2(savedStderr, STDERR_FILENO);
            close(savedStdin);
            close(savedStdout);
            close(savedStderr);
            std::cerr << "无法打开文件: " << targets[i] << std::endl;
            return ExecResult(ExecResultType::NORMAL, makeIntValue(1));
        }
        
        dup2(fd, targetFd);
        close(fd);
    }
    
    // 执行命令
    ExecResult cmdResult = executeNode(node->command);
    
    // 恢复原始 fd
    dup2(savedStdin, STDIN_FILENO);
    dup2(savedStdout, STDOUT_FILENO);
    dup2(savedStderr, STDERR_FILENO);
    close(savedStdin);
    close(savedStdout);
    close(savedStderr);
    
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
}

} // namespace wash
