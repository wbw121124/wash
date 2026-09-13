/**
 * @file parser.h
 * @brief wash 语法分析器
 * 
 * 将 Token 流转换为 AST（抽象语法树）。
 * 
 * @author wash
 * @date 2026-09-12
 */

#ifndef WASH_PARSER_H
#define WASH_PARSER_H

#include "types.h"
#include "lexer.h"
#include <string>
#include <vector>

namespace wash {

/**
 * @brief 语法分析器类
 */
class Parser {
public:
    /**
     * @brief 构造函数
     * @param tokens Token 列表
     * @param filename 文件名（用于错误报告）
     */
    Parser(const std::vector<Token>& tokens, const std::string& filename = "<input>");
    
    /**
     * @brief 解析并返回 AST
     * @return 程序节点
     */
    ASTPtr parse();
    
    /**
     * @brief 获取错误信息
     * @return 错误信息
     */
    const std::string& getError() const;
    
    /**
     * @brief 检查是否有错误
     * @return 是否有错误
     */
    bool hasError() const;
    
private:
    /**
     * @brief 解析语句
     * @return AST 节点
     */
    ASTPtr parseStatement();
    
    /**
     * @brief 解析管道表达式
     * @return 管道节点或普通表达式节点
     */
    ASTPtr parsePipeExpression();
    
    /**
     * @brief 解析代码块
     * @return 代码块节点
     */
    ASTPtr parseBlock();
    
    /**
     * @brief 解析赋值语句
     * @return 赋值节点或表达式节点
     */
    ASTPtr parseAssignment();
    
    /**
     * @brief 解析表达式
     * @return 表达式节点
     */
    ASTPtr parseExpression();
    
    /**
     * @brief 解析逗号表达式
     * @return 表达式节点
     */
    ASTPtr parseComma();
    
    /**
     * @brief 解析三目表达式
     * @return 表达式节点
     */
    ASTPtr parseTernary();
    
    /**
     * @brief 解析范围表达式
     * @return 表达式节点
     */
    ASTPtr parseRange();
    
    /**
     * @brief 解析逻辑或表达式
     * @return 表达式节点
     */
    ASTPtr parseLogicalOr();
    
    /**
     * @brief 解析逻辑与表达式
     * @return 表达式节点
     */
    ASTPtr parseLogicalAnd();
    
    /**
     * @brief 解析按位或表达式
     * @return 表达式节点
     */
    ASTPtr parseBitwiseOr();
    
    /**
     * @brief 解析按位异或表达式
     * @return 表达式节点
     */
    ASTPtr parseBitwiseXor();
    
    /**
     * @brief 解析按位与表达式
     * @return 表达式节点
     */
    ASTPtr parseBitwiseAnd();
    
    /**
     * @brief 解析相等性表达式
     * @return 表达式节点
     */
    ASTPtr parseEquality();
    
    /**
     * @brief 解析关系表达式
     * @return 表达式节点
     */
    ASTPtr parseRelational();
    
    /**
     * @brief 解析移位表达式
     * @return 表达式节点
     */
    ASTPtr parseShift();
    
    /**
     * @brief 解析加法表达式
     * @return 表达式节点
     */
    ASTPtr parseAdditive();
    
    /**
     * @brief 解析乘法表达式
     * @return 表达式节点
     */
    ASTPtr parseMultiplicative();
    
    /**
     * @brief 解析一元表达式
     * @return 表达式节点
     */
    ASTPtr parseUnary();
    
    /**
     * @brief 解析后缀表达式
     * @return 表达式节点
     */
    ASTPtr parsePostfix();
    
    /**
     * @brief 解析主表达式
     * @return 表达式节点
     */
    ASTPtr parsePrimary();
    
    /**
     * @brief 解析 if 语句
     * @return if 节点
     */
    ASTPtr parseIfStatement();
    
    /**
     * @brief 解析 for 循环
     * @return for 节点
     */
    ASTPtr parseForStatement();
    
    /**
     * @brief 解析 while 循环
     * @return while 节点
     */
    ASTPtr parseWhileStatement();
    
    /**
     * @brief 解析函数定义
     * @return 函数定义节点
     */
    ASTPtr parseFunctionDef();
    
    /**
     * @brief 解析 return 语句
     * @return return 节点
     */
    ASTPtr parseReturnStatement();
    
    /**
     * @brief 解析 break 语句
     * @return break 节点
     */
    ASTPtr parseBreakStatement();
    
    /**
     * @brief 解析 continue 语句
     * @return continue 节点
     */
    ASTPtr parseContinueStatement();
    
    /**
     * @brief 解析函数调用
     * @param name 函数名
     * @return 函数调用节点
     */
    ASTPtr parseFunctionCall(const std::string& name);
    
    /**
     * @brief 解析参数列表
     * @return 参数列表
     */
    std::vector<ASTPtr> parseArgumentList();
    
    /**
     * @brief 获取当前 Token
     * @return 当前 Token
     */
    Token& current();
    
    /**
     * @brief 获取下一个 Token
     * @return 下一个 Token
     */
    Token& advance();
    
    /**
     * @brief 查看下一个 Token
     * @return 下一个 Token
     */
    Token& peek();
    
    /**
     * @brief 检查当前 Token 类型是否匹配
     * @param type Token 类型
     * @return 是否匹配
     */
    bool check(TokenType type);
    
    /**
     * @brief 检查当前 Token 值是否匹配
     * @param value Token 值
     * @return 是否匹配
     */
    bool checkValue(const std::string& value);
    
    /**
     * @brief 匹配并消费指定类型的 Token
     * @param type Token 类型
     * @return 是否匹配
     */
    bool match(TokenType type);
    
    /**
     * @brief 匹配并消费指定值的 Token
     * @param value Token 值
     * @return 是否匹配
     */
    bool matchValue(const std::string& value);
    
    /**
     * @brief 期望指定类型的 Token
     * @param type Token 类型
     * @param message 错误信息
     * @return Token
     */
    Token expect(TokenType type, const std::string& message);
    
    /**
     * @brief 期望指定值的 Token
     * @param value Token 值
     * @param message 错误信息
     * @return Token
     */
    Token expectValue(const std::string& value, const std::string& message);
    
    /**
     * @brief 报告错误
     * @param message 错误信息
     */
    void error(const std::string& message);
    
    /**
     * @brief 跳过换行和分号
     */
    void skipNewlines();
    
    /**
     * @brief 检查是否为语句结束符
     * @return 是否为语句结束符
     */
    bool isStatementEnd();
    
    std::vector<Token> tokens_;      ///< Token 列表
    size_t pos_;                     ///< 当前位置
    std::string filename_;           ///< 文件名
    std::string error_;              ///< 错误信息
    bool hasError_;                  ///< 是否有错误
};

} // namespace wash

#endif // WASH_PARSER_H
