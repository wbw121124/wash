/**
 * @file lexer.cpp
 * @brief wash 词法分析器实现
 * 
 * @author wash
 * @date 2026-09-12
 */

#include "lexer.h"
#include <cctype>
#include <stdexcept>
#include <libintl.h>

#define _(STRING) gettext(STRING)

namespace wash {

static const std::vector<std::string> RESERVED_KEYWORDS = {
    "env", "if", "elif", "else", "for", "while", "in",
    "return", "break", "continue", "true", "false", "calc"
};

Lexer::Lexer(const std::string& source, const std::string& filename)
    : source_(source), filename_(filename), pos_(0), line_(1), column_(1),
      tokenStart_(0), tokenLine_(1), tokenColumn_(1), tokenIndex_(0),
      lastTokenType_(TokenType::NONE_TOK) {}

std::vector<Token> Lexer::tokenize() {
    tokens_.clear();
    tokenIndex_ = 0;
    lastTokenType_ = TokenType::NONE_TOK;
    
    while (!isAtEnd()) {
        Token token = scanToken();
        lastTokenType_ = token.type;
        tokens_.push_back(token);
        if (token.type == TokenType::EOF_TOKEN) break;
    }
    
    // 相邻字符串自动连接: "hello " "world" -> "hello world"
    std::vector<Token> merged;
    for (size_t i = 0; i < tokens_.size(); ++i) {
        if (tokens_[i].type == TokenType::STRING && !merged.empty() && 
            merged.back().type == TokenType::STRING) {
            merged.back().value += tokens_[i].value;
        } else {
            merged.push_back(tokens_[i]);
        }
    }
    
    return merged;
}

Token Lexer::nextToken() {
    if (tokenIndex_ < tokens_.size()) return tokens_[tokenIndex_++];
    return makeToken(TokenType::EOF_TOKEN);
}

Token Lexer::peekToken() {
    if (tokenIndex_ < tokens_.size()) return tokens_[tokenIndex_];
    return makeToken(TokenType::EOF_TOKEN);
}

bool Lexer::hasMore() const { return tokenIndex_ < tokens_.size(); }
size_t Lexer::getCurrentLine() const { return line_; }
size_t Lexer::getCurrentColumn() const { return column_; }
const std::string& Lexer::getFilename() const { return filename_; }

bool Lexer::hasError() const {
    for (const auto& t : tokens_) {
        if (t.type == TokenType::ERROR_TOK) return true;
    }
    return false;
}

bool Lexer::isReservedKeyword(const std::string& word) {
    for (const auto& kw : RESERVED_KEYWORDS) {
        if (kw == word) return true;
    }
    return false;
}

bool Lexer::isValidVariableName(const std::string& name) {
    if (name.empty()) return false;
    if (!std::isalpha(name[0]) && name[0] != '_') return false;
    for (size_t i = 1; i < name.size(); ++i) {
        char c = name[i];
        if (!std::isalnum(c) && c != '_' && c != '-') return false;
    }
    return !isReservedKeyword(name);
}

bool Lexer::isValidFunctionName(const std::string& name) {
    return isValidVariableName(name);
}

Token Lexer::scanToken() {
    skipWhitespace();
    if (isAtEnd()) return makeToken(TokenType::EOF_TOKEN);
    
    tokenStart_ = pos_;
    tokenLine_ = line_;
    tokenColumn_ = column_;
    
    char c = current();
    
    if (std::isdigit(c)) return scanNumber();
    if (std::isalpha(c) || c == '_') return scanIdentifier();
    if (c == '%') {
        // 上下文判断：% 是变量前缀还是取模运算符
        // 如果上一个 token 是数字、标识符、右括号、右方括号，则 % 是取模
        bool isModulo = (lastTokenType_ == TokenType::NUMBER ||
                        lastTokenType_ == TokenType::IDENTIFIER ||
                        lastTokenType_ == TokenType::VAR ||
                        lastTokenType_ == TokenType::ENV_VAR ||
                        lastTokenType_ == TokenType::RPAREN ||
                        lastTokenType_ == TokenType::RBRACKET ||
                        lastTokenType_ == TokenType::TRUE_KW ||
                        lastTokenType_ == TokenType::FALSE_KW);
        if (isModulo) {
            advance();
            return makeToken(TokenType::PERCENT, "%");
        }
        return scanVariable();
    }
    if (c == '$') return scanCommand();
    if (c == '@') return scanRedirect();
    if (c == '\'') return scanSingleQuoteString();
    if (c == '"') return scanDoubleQuoteString();
    if (c == '`') return scanBacktickString();
    
    if (c == '#') {
        if (peek() == '[' && peek(1) == '[') return scanBlockComment();
        skipLineComment();
        return scanToken();
    }
    if (c == '/' && peek() == '*') return scanBlockComment();
    
    advance();
    
    switch (c) {
        case '+': return makeToken(TokenType::PLUS, "+");
        case '-': return makeToken(TokenType::MINUS, "-");
        case '*': return makeToken(TokenType::STAR, "*");
        case '/': return makeToken(TokenType::SLASH, "/");
        case '%': return makeToken(TokenType::PERCENT, "%");
        case '^': return makeToken(TokenType::CARET, "^");
        case '&':
            if (current() == '&') { advance(); return makeToken(TokenType::AND, "&&"); }
            return makeToken(TokenType::AMPERSAND, "&");
        case '|':
            if (current() == '|') { advance(); return makeToken(TokenType::OR, "||"); }
            return makeToken(TokenType::PIPE, "|");
        case '~': return makeToken(TokenType::TILDE, "~");
        case '<':
            if (current() == '=') { advance(); return makeToken(TokenType::LESS_EQ, "<="); }
            if (current() == '<') { advance(); return makeToken(TokenType::LSHIFT, "<<"); }
            return makeToken(TokenType::LESS, "<");
        case '>':
            if (current() == '=') { advance(); return makeToken(TokenType::GREATER_EQ, ">="); }
            if (current() == '>') { advance(); return makeToken(TokenType::RSHIFT, ">>"); }
            return makeToken(TokenType::GREATER, ">");
        case '=':
            if (current() == '=') { advance(); return makeToken(TokenType::EQUAL, "=="); }
            return makeToken(TokenType::ASSIGN, "=");
        case '!':
            if (current() == '=') { advance(); return makeToken(TokenType::NOT_EQUAL, "!="); }
            return makeToken(TokenType::NOT, "!");
        case '?': return makeToken(TokenType::QUESTION, "?");
        case '(': return makeToken(TokenType::LPAREN, "(");
        case ')': return makeToken(TokenType::RPAREN, ")");
        case '{': return makeToken(TokenType::LBRACE, "{");
        case '}': return makeToken(TokenType::RBRACE, "}");
        case '[': return makeToken(TokenType::LBRACKET, "[");
        case ']': return makeToken(TokenType::RBRACKET, "]");
        case ';': return makeToken(TokenType::SEMICOLON, ";");
        case ',': return makeToken(TokenType::COMMA, ",");
        case ':': return makeToken(TokenType::COLON, ":");
        case '.': return makeToken(TokenType::DOT, ".");
        case '\n': return makeToken(TokenType::NEWLINE_TOK, "\n");
        default: return makeError(std::string("意外字符: ") + c);
    }
}

Token Lexer::scanNumber() {
    std::string num;
    bool hasDot = false;
    while (!isAtEnd() && (std::isdigit(current()) || current() == '.')) {
        if (current() == '.') {
            // 检查是否为范围运算符 .. 或 ..<
            if (pos_ + 1 < source_.size() && source_[pos_ + 1] == '.') {
                break; // 不消费第二个点，留给范围运算符处理
            }
            if (hasDot) break;
            hasDot = true;
        }
        num += current();
        advance();
    }
    return makeToken(TokenType::NUMBER, num);
}

Token Lexer::scanIdentifier() {
    std::string id;
    while (!isAtEnd() && (std::isalnum(current()) || current() == '_' || current() == '-')) {
        id += current();
        advance();
    }
    
    if (id == "if") return makeToken(TokenType::IF, id);
    if (id == "elif") return makeToken(TokenType::ELIF, id);
    if (id == "else") return makeToken(TokenType::ELSE, id);
    if (id == "for") return makeToken(TokenType::FOR, id);
    if (id == "while") return makeToken(TokenType::WHILE, id);
    if (id == "in") return makeToken(TokenType::IN, id);
    if (id == "return") return makeToken(TokenType::RETURN_KW, id);
    if (id == "break") return makeToken(TokenType::BREAK_KW, id);
    if (id == "continue") return makeToken(TokenType::CONTINUE_KW, id);
    if (id == "true") return makeToken(TokenType::TRUE_KW, id);
    if (id == "false") return makeToken(TokenType::FALSE_KW, id);
    if (id == "calc") return makeToken(TokenType::CALC, id);
    
    return makeToken(TokenType::IDENTIFIER, id);
}

Token Lexer::scanSingleQuoteString() {
    advance();
    std::string str;
    while (!isAtEnd() && current() != '\'') {
        if (current() == '\\') {
            advance();
            if (isAtEnd()) return makeError("字符串未结束");
            if (current() == '\'') str += '\'';
            else if (current() == '\\') str += '\\';
            else { str += '\\'; str += current(); }
        } else {
            str += current();
        }
        advance();
    }
    if (isAtEnd()) return makeError("字符串未结束");
    advance();
    return makeToken(TokenType::STRING, str);
}

Token Lexer::scanDoubleQuoteString() {
    advance();
    std::string str;
    while (!isAtEnd() && current() != '"') {
        if (current() == '\\') {
            advance();
            if (isAtEnd()) return makeError("字符串未结束");
            char escaped = escapeChar(current());
            if (escaped == '\0') return makeError(std::string("无效转义序列: \\") + current());
            str += escaped;
        } else if (current() == '%' && peek() == '{') {
            str += "%{"; advance(); advance();
            while (!isAtEnd() && current() != '}') { str += current(); advance(); }
            if (!isAtEnd()) { str += '}'; advance(); }
        } else {
            str += current();
        }
        advance();
    }
    if (isAtEnd()) return makeError("字符串未结束");
    advance();
    return makeToken(TokenType::STRING, str);
}

Token Lexer::scanBacktickString() {
    advance();
    std::string str;
    while (!isAtEnd() && current() != '`') {
        if (current() == '\\') {
            advance();
            if (isAtEnd()) return makeError("字符串未结束");
            char escaped = escapeChar(current());
            if (escaped == '\0') return makeError(std::string("无效转义序列: \\") + current());
            str += escaped;
        } else if (current() == '%' && peek() == '{') {
            str += "%{"; advance(); advance();
            while (!isAtEnd() && current() != '}') { str += current(); advance(); }
            if (!isAtEnd()) { str += '}'; advance(); }
        } else {
            if (current() == '\n') { line_++; column_ = 1; }
            str += current();
        }
        advance();
    }
    if (isAtEnd()) return makeError("字符串未结束");
    advance();
    return makeToken(TokenType::STRING, str);
}

Token Lexer::scanBlockComment() {
    if (current() == '#' && peek() == '[' && peek(1) == '[') {
        advance(); advance(); advance(); advance();
        while (!isAtEnd()) {
            if (current() == ']' && peek() == ']' && peek(1) == '#') {
                advance(); advance(); advance();
                return makeToken(TokenType::NONE_TOK);
            }
            if (current() == '\n') { line_++; column_ = 1; }
            advance();
        }
    } else if (current() == '/' && peek() == '*') {
        advance(); advance();
        while (!isAtEnd()) {
            if (current() == '*' && peek() == '/') {
                advance(); advance();
                return makeToken(TokenType::NONE_TOK);
            }
            if (current() == '\n') { line_++; column_ = 1; }
            advance();
        }
    }
    return makeError("注释未结束");
}

Token Lexer::scanVariable() {
    advance();
    
    if (current() == 'e' && peek(1) == 'n' && peek(2) == 'v') {
        size_t savedPos = pos_;
        size_t savedLine = line_;
        size_t savedCol = column_;
        advance(); advance(); advance();
        if (current() == '.' || current() == ':') {
            advance();
            std::string varName;
            while (!isAtEnd() && (std::isalnum(current()) || current() == '_' || current() == '-')) {
                varName += current(); advance();
            }
            return makeToken(TokenType::ENV_VAR, varName);
        } else {
            // %env 无后缀，生成 ENV_VIEW token
            return makeToken(TokenType::ENV_VIEW, "env");
        }
    }
    
    std::string varName;
    while (!isAtEnd() && (std::isalnum(current()) || current() == '_' || current() == '-')) {
        varName += current(); advance();
    }
    if (varName.empty()) return makeError("变量名为空");
    return makeToken(TokenType::VAR, varName);
}

Token Lexer::scanCommand() {
    advance();
    if (current() == '(') return scanCommandOutput();
    if (current() == '"') {
        advance();
        std::string cmd;
        while (!isAtEnd() && current() != '"') {
            if (current() == '\\') { advance(); if (!isAtEnd()) cmd += current(); }
            else cmd += current();
            advance();
        }
        if (!isAtEnd()) advance();
        return makeToken(TokenType::CMD_STRING, cmd);
    }
    std::string cmd;
    while (!isAtEnd() && !std::isspace(current()) && current() != '|' &&
           current() != ';' && current() != '\n' && current() != '@') {
        cmd += current(); advance();
    }
    if (cmd.empty()) return makeError("命令名为空");
    return makeToken(TokenType::COMMAND, cmd);
}

Token Lexer::scanCommandOutput() {
    advance();
    std::string cmd;
    int depth = 1;
    while (!isAtEnd() && depth > 0) {
        if (current() == '(') depth++;
        else if (current() == ')') { depth--; if (depth == 0) break; }
        cmd += current(); advance();
    }
    if (!isAtEnd()) advance();
    return makeToken(TokenType::CMD_OUTPUT, cmd);
}

Token Lexer::scanRedirect() {
    advance();
    if (current() != '(') return makeError("重定向格式错误，应为 @(...)");
    advance();
    std::string content;
    int depth = 1;
    while (!isAtEnd() && depth > 0) {
        if (current() == '(') depth++;
        else if (current() == ')') { depth--; if (depth == 0) break; }
        content += current(); advance();
    }
    if (!isAtEnd()) advance();
    return makeToken(TokenType::REDIRECT, content);
}

void Lexer::skipWhitespace() {
    while (!isAtEnd() && std::isspace(current()) && current() != '\n') {
        // 行续：\ + \n
        if (current() == '\\' && pos_ + 1 < source_.size() && source_[pos_ + 1] == '\n') {
            advance(); // 跳过 \
            advance(); // 跳过 \n
            line_++;
            column_ = 1;
            continue;
        }
        advance();
    }
}

void Lexer::skipLineComment() {
    while (!isAtEnd() && current() != '\n') advance();
}

char Lexer::current() const { return pos_ >= source_.size() ? '\0' : source_[pos_]; }
char Lexer::peek() const { return peek(0); }
char Lexer::peek(size_t n) const { size_t idx = pos_ + n; return idx >= source_.size() ? '\0' : source_[idx]; }

void Lexer::advance() {
    if (pos_ < source_.size()) {
        if (source_[pos_] == '\n') { line_++; column_ = 1; }
        else column_++;
        pos_++;
    }
}

void Lexer::back() {
    if (pos_ > 0) {
        pos_--;
        if (source_[pos_] == '\n') { line_--; column_ = 1; }
        else column_--;
    }
}

bool Lexer::isAtEnd() const { return pos_ >= source_.size(); }
Token Lexer::makeToken(TokenType type, const std::string& value) { return Token(type, value, tokenLine_, tokenColumn_); }
Token Lexer::makeError(const std::string& message) { return Token(TokenType::ERROR_TOK, message, tokenLine_, tokenColumn_); }

char Lexer::escapeChar(char c) const {
    switch (c) {
        case 'n': return '\n';
        case 't': return '\t';
        case 'r': return '\r';
        case '\\': return '\\';
        case '"': return '"';
        case '%': return '%';
        case '$': return '$';
        default: return '\0';
    }
}

} // namespace wash
