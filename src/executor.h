/**
 * @file executor.h
 * @brief wash 执行器
 * 
 * 执行 AST 并返回结果。
 * 
 * @author wash
 * @date 2026-09-12
 */

#ifndef WASH_EXECUTOR_H
#define WASH_EXECUTOR_H

#include "types.h"
#include <string>
#include <vector>
#include <map>
#include <functional>

namespace wash {

/**
 * @brief 函数类型
 */
using Function = std::function<ExecResult(const std::vector<Value>&)>;

/**
 * @brief 执行器类
 */
class Executor {
public:
    /**
     * @brief 构造函数
     */
    Executor();
    
    /**
     * @brief 设置脚本命令行参数
     * @param argc 参数个数
     * @param argv 参数数组
     */
    void setScriptArgs(int argc, const char* const* argv);
    
    /**
     * @brief 执行程序
     * @param program 程序节点
     * @return 执行结果
     */
    ExecResult execute(ASTPtr program);
    
    /**
     * @brief 设置变量
     * @param name 变量名
     * @param value 变量值
     */
    void setVariable(const std::string& name, const Value& value);
    
    /**
     * @brief 获取变量
     * @param name 变量名
     * @return 变量值
     */
    Value getVariable(const std::string& name);
    
    /**
     * @brief 设置环境变量
     * @param name 变量名
     * @param value 变量值
     */
    void setEnvVariable(const std::string& name, const Value& value);
    
    /**
     * @brief 获取环境变量
     * @param name 变量名
     * @return 变量值
     */
    Value getEnvVariable(const std::string& name);
    
    /**
     * @brief 获取所有环境变量
     * @return 环境变量映射
     */
    const std::map<std::string, Value>& getAllEnvVariables() const;
    
    /**
     * @brief 定义函数
     * @param name 函数名
     * @param func 函数实现
     */
    void defineFunction(const std::string& name, Function func);
    
    /**
     * @brief 调用函数
     * @param name 函数名
     * @param args 参数列表
     * @return 执行结果
     */
    ExecResult callFunction(const std::string& name, const std::vector<Value>& args);
    
    /**
     * @brief 进入作用域
     */
    void enterScope();
    
    /**
     * @brief 退出作用域
     */
    void exitScope();
    
    /**
     * @brief 获取最后退出码
     * @return 退出码
     */
    int getExitCode() const;
    
    /**
     * @brief 设置退出码
     * @param code 退出码
     */
    void setExitCode(int code);
    
    /**
     * @brief 检查是否有退出请求
     * @return 是否有退出请求
     */
    bool shouldExit() const;
    
    /**
     * @brief 请求退出
     * @param code 退出码
     */
    void requestExit(int code = 0);
    
private:
    /**
     * @brief 执行节点
     * @param node AST 节点
     * @return 执行结果
     */
    ExecResult executeNode(ASTPtr node);
    
    /**
     * @brief 执行语句列表
     * @param statements 语句列表
     * @return 执行结果
     */
    ExecResult executeStatements(const std::vector<ASTPtr>& statements);
    
    /**
     * @brief 执行字面量
     * @param node 字面量节点
     * @return 执行结果
     */
    ExecResult executeLiteral(LiteralNode* node);
    
    /**
     * @brief 执行变量引用
     * @param node 变量节点
     * @return 执行结果
     */
    ExecResult executeVariable(VariableNode* node);
    
    /**
     * @brief 执行环境变量引用
     * @param node 环境变量节点
     * @return 执行结果
     */
    ExecResult executeEnvVariable(EnvVariableNode* node);
    
    /**
     * @brief 执行二元运算
     * @param node 二元运算节点
     * @return 执行结果
     */
    ExecResult executeBinaryOp(BinaryOpNode* node);
    
    /**
     * @brief 执行一元运算
     * @param node 一元运算节点
     * @return 执行结果
     */
    ExecResult executeUnaryOp(UnaryOpNode* node);
    
    /**
     * @brief 执行三元运算
     * @param node 三元运算节点
     * @return 执行结果
     */
    ExecResult executeTernaryOp(TernaryOpNode* node);
    
    /**
     * @brief 执行函数调用
     * @param node 函数调用节点
     * @return 执行结果
     */
    ExecResult executeFunctionCall(FunctionCallNode* node);
    
    /**
     * @brief 执行赋值
     * @param node 赋值节点
     * @return 执行结果
     */
    ExecResult executeAssignment(AssignmentNode* node);
    
    /**
     * @brief 执行环境变量赋值
     * @param node 环境变量赋值节点
     * @return 执行结果
     */
    ExecResult executeEnvAssignment(EnvAssignmentNode* node);
    
    /**
     * @brief 执行命令
     * @param node 命令节点
     * @return 执行结果
     */
    ExecResult executeCommand(CommandExecNode* node);
    
    /**
     * @brief 执行命令输出替换
     * @param node 命令输出节点
     * @return 执行结果
     */
    ExecResult executeCommandOutput(CommandOutputNode* node);
    
    /**
     * @brief 执行 if 语句
     * @param node if 节点
     * @return 执行结果
     */
    ExecResult executeIf(IfNode* node);
    
    /**
     * @brief 执行 for 循环
     * @param node for 节点
     * @return 执行结果
     */
    ExecResult executeFor(ForNode* node);
    
    /**
     * @brief 执行 while 循环
     * @param node while 节点
     * @return 执行结果
     */
    ExecResult executeWhile(WhileNode* node);
    
    /**
     * @brief 执行 return 语句
     * @param node return 节点
     * @return 执行结果
     */
    ExecResult executeReturn(ReturnNode* node);
    
    /**
     * @brief 执行函数定义
     * @param node 函数定义节点
     * @return 执行结果
     */
    ExecResult executeFunctionDef(FunctionDefNode* node);
    
    /**
     * @brief 执行 lambda 定义
     * @param node lambda 定义节点
     * @return 执行结果
     */
    ExecResult executeLambdaDef(LambdaDefNode* node);
    
    /**
     * @brief 生成范围序列
     * @param range 范围表达式
     * @return 值列表
     */
    std::vector<Value> generateRange(ASTPtr range);
    
    /**
     * @brief 解析重定向字符串
     * @param content 重定向内容
     * @return 重定向目标列表
     */
    std::vector<std::string> parseRedirectContent(const std::string& content);
    
    /**
     * @brief 执行外部命令
     * @param cmd 命令
     * @param args 参数列表
     * @return 退出码
     */
    int executeExternalCommand(const std::string& cmd, const std::vector<std::string>& args);
    
    /**
     * @brief 执行管道命令
     * @param commands 命令列表
     * @return 退出码
     */
    int executePipe(const std::vector<std::string>& commands);
    
    /**
     * @brief 输出值到 stdout
     * @param value 值
     */
    void outputToStdout(const Value& value);
    
    /**
     * @brief 输出值到 stderr
     * @param value 值
     */
    void outputToStderr(const Value& value);
    
    /**
     * @brief 注册内建函数
     */
    void registerBuiltinFunctions();
    
    std::shared_ptr<Scope> currentScope_;    ///< 当前作用域
    std::map<std::string, Value> envVars_;   ///< 环境变量
    std::map<std::string, Function> functions_;  ///< 函数表
    int exitCode_;                           ///< 退出码
    bool exitRequested_;                     ///< 是否请求退出
    int scriptArgc_;                         ///< 脚本命令行参数个数
    std::vector<std::string> scriptArgv_;    ///< 脚本命令行参数
};

} // namespace wash

#endif // WASH_EXECUTOR_H
