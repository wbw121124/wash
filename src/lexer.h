/**
 * @file lexer.h
 * @brief wash 词法分析器
 * 
 * 将源码字符串转换为 Token 流。
 * 
 * @author wash
 * @date 2026-09-12
 */

#ifndef WASH_LEXER_H
#define WASH_LEXER_H

#include "types.h"
#include <string>
#include <vector>

namespace wash {

/**
 * @brief 词法分析器类
 */
class Lexer {
public:
    /**
     * @brief 构造函数
     * @param source 源码字符串
     * @param filename 文件名（用于错误报告）
     */
    Lexer(const std::string& source, const std::string& filename = "<input>");
    
    /**
     * @brief 获取所有 Token
     * @return Token 列表
     */
    std::vector<Token> tokenize();
    
    /**
     * @brief 获取下一个 Token
     * @return 下一个 Token
     */
    Token nextToken();
    
    /**
     * @brief 查看下一个 Token（不消耗）
     * @return 下一个 Token
     */
    Token peekToken();
    
    /**
     * @brief 检查是否已无更多 Token
     * @return 是否已无更多 Token
     */
    bool hasMore() const;
    
    /**
     * @brief 检查是否有错误 Token
     * @return 是否有错误
     */
    bool hasError() const;
    
    /**
     * @brief 获取当前行号
     * @return 行号
     */
    size_t getCurrentLine() const;
    
    /**
     * @brief 获取当前列号
     * @return 列号
     */
    size_t getCurrentColumn() const;
    
    /**
     * @brief 获取文件名
     * @return 文件名
     */
    const std::string& getFilename() const;
    
    /**
     * @brief 检查关键字是否为保留关键字
     * @param word 关键字
     * @return 是否为保留关键字
     */
    static bool isReservedKeyword(const std::string& word);
    
    /**
     * @brief 检查字符串是否为有效变量名
     * @param name 变量名
     * @return 是否有效
     */
    static bool isValidVariableName(const std::string& name);
    
    /**
     * @brief 检查字符串是否为有效函数名
     * @param name 函数名
     * @return 是否有效
     */
    static bool isValidFunctionName(const std::string& name);
    
private:
    /**
     * @brief 扫描并返回下一个 Token
     * @return Token
     */
    Token scanToken();
    
    /**
     * @brief 扫描数字
     * @return Token
     */
    Token scanNumber();
    
    /**
     * @brief 扫描标识符或关键字
     * @return Token
     */
    Token scanIdentifier();
    
    /**
     * @brief 扫描字符串（单引号）
     * @return Token
     */
    Token scanSingleQuoteString();
    
    /**
     * @brief 扫描字符串（双引号）
     * @return Token
     */
    Token scanDoubleQuoteString();
    
    /**
     * @brief 扫描字符串（反引号）
     * @return Token
     */
    Token scanBacktickString();
    
    /**
     * @brief 扫描块注释
     * @return Token（可能为 NONE）
     */
    Token scanBlockComment();
    
    /**
     * @brief 扫描变量（%var）
     * @return Token
     */
    Token scanVariable();
    
    /**
     * @brief 扫描命令（$cmd）
     * @return Token
     */
    Token scanCommand();
    
    /**
     * @brief 扫描命令输出替换（$(...)）
     * @return Token
     */
    Token scanCommandOutput();
    
    /**
     * @brief 扫描重定向（@(...)）
     * @return Token
     */
    Token scanRedirect();
    
    /**
     * @brief 跳过空白字符
     */
    void skipWhitespace();
    
    /**
     * @brief 跳过行注释
     */
    void skipLineComment();
    
    /**
     * @brief 获取当前字符
     * @return 当前字符
     */
    char current() const;
    
    /**
     * @brief 获取下一个字符
     * @return 下一个字符
     */
    char peek() const;
    
    /**
     * @brief 获取第 n 个前瞻字符
     * @param n 前瞻位置
     * @return 字符
     */
    char peek(size_t n) const;
    
    /**
     * @brief 前进一个字符
     */
    void advance();
    
    /**
     * @brief 后退一个字符
     */
    void back();
    
    /**
     * @brief 检查是否到达末尾
     * @return 是否到达末尾
     */
    bool isAtEnd() const;
    
    /**
     * @brief 创建 Token
     * @param type Token 类型
     * @param value Token 值
     * @return Token
     */
    Token makeToken(TokenType type, const std::string& value = "");
    
    /**
     * @brief 创建错误 Token
     * @param message 错误信息
     * @return Token
     */
    Token makeError(const std::string& message);
    
    /**
     * @brief 处理双引号转义字符
     * @param c 字符
     * @return 转义后的字符，返回 '\0' 表示无效转义
     */
    char escapeChar(char c) const;
    
    std::string source_;            ///< 源码字符串
    std::string filename_;          ///< 文件名
    size_t pos_;                    ///< 当前位置
    size_t line_;                   ///< 当前行号
    size_t column_;                 ///< 当前列号
    size_t tokenStart_;             ///< 当前 Token 起始位置
    size_t tokenLine_;              ///< 当前 Token 起始行号
    size_t tokenColumn_;            ///< 当前 Token 起始列号
    std::vector<Token> tokens_;     ///< Token 缓冲区
    size_t tokenIndex_;             ///< 当前 Token 索引
    TokenType lastTokenType_;       ///< 上一个 Token 类型（用于 % 上下文判断）
};

} // namespace wash

#endif // WASH_LEXER_H
