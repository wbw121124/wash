/**
 * @file parser.cpp
 * @brief wash 语法分析器实现
 * 
 * @author wash
 * @date 2026-09-12
 */

#include "parser.h"
#include "types.h"
#include <stdexcept>
#include <sstream>
#include <cstdint>
#include <libintl.h>

#define _(STRING) gettext(STRING)

namespace wash {

Parser::Parser(const std::vector<Token>& tokens, const std::string& filename)
    : tokens_(tokens), pos_(0), filename_(filename), hasError_(false) {}

ASTPtr Parser::parse() {
    auto program = std::make_shared<ProgramNode>();
    
    skipNewlines();
    
    while (pos_ < tokens_.size() && current().type != TokenType::EOF_TOKEN) {
        ASTPtr stmt = parseStatement();
        if (stmt) {
            program->statements.push_back(stmt);
        }
        
        if (hasError_) {
            return nullptr;
        }
        
        skipNewlines();
    }
    
    return program;
}

const std::string& Parser::getError() const {
    return error_;
}

bool Parser::hasError() const {
    return hasError_;
}

ASTPtr Parser::parseStatement() {
    Token& tok = current();
    
    // 跳过换行和分号
    if (tok.type == TokenType::NEWLINE_TOK || tok.type == TokenType::SEMICOLON) {
        advance();
        return nullptr;
    }
    
    // if 语句
    if (tok.type == TokenType::IF) {
        return parseIfStatement();
    }
    
    // for 循环
    if (tok.type == TokenType::FOR) {
        return parseForStatement();
    }
    
    // while 循环
    if (tok.type == TokenType::WHILE) {
        return parseWhileStatement();
    }
    
    // return 语句
    if (tok.type == TokenType::RETURN_KW) {
        return parseReturnStatement();
    }
    
    // break 语句
    if (tok.type == TokenType::BREAK_KW) {
        return parseBreakStatement();
    }
    
    // continue 语句
    if (tok.type == TokenType::CONTINUE_KW) {
        return parseContinueStatement();
    }
    
    // 函数定义：f():{...} 或 f=():{...}
    if (tok.type == TokenType::IDENTIFIER && pos_ + 1 < tokens_.size()) {
        size_t savedPos = pos_;
        
        // 形式 1: f():{...}
        if (tokens_[pos_ + 1].type == TokenType::LPAREN) {
            advance(); advance();  // 跳过函数名和 (
            // 检查参数列表
            while (current().type != TokenType::RPAREN && 
                   current().type != TokenType::EOF_TOKEN) {
                advance();
            }
            if (current().type == TokenType::RPAREN) {
                advance();  // 跳过 )
                if (current().type == TokenType::COLON) {
                    advance();  // 跳过 :
                    if (current().type == TokenType::LBRACE) {
                        pos_ = savedPos;
                        return parseFunctionDef();
                    }
                }
            }
            pos_ = savedPos;
        }
        
        // 形式 2: f=():{...}
        if (tokens_[pos_ + 1].type == TokenType::ASSIGN && pos_ + 2 < tokens_.size() &&
            tokens_[pos_ + 2].type == TokenType::LPAREN) {
            // 不预检查，直接交给 parseFunctionDef 处理
            // parseFunctionDef 会检查 =():{ 模式
            return parseFunctionDef();
        }
    }
    
    // 赋值语句或表达式
    return parsePipeExpression();
}

ASTPtr Parser::parsePipeExpression() {
    ASTPtr left = parseAssignment();
    
    // 处理管道 $cmd | %var 或 $cmd1 | $cmd2
    if (check(TokenType::PIPE)) {
        advance(); // 跳过 |
        ASTPtr right = parseAssignment();
        return std::make_shared<PipeExprNode>(left, right, left->line, left->column);
    }
    
    return left;
}

ASTPtr Parser::parseBlock() {
    if (!match(TokenType::LBRACE)) {
        error("期望 '{'");
        return nullptr;
    }
    
    auto block = std::make_shared<BlockNode>();
    
    skipNewlines();
    
    while (pos_ < tokens_.size() && current().type != TokenType::RBRACE) {
        ASTPtr stmt = parseStatement();
        if (stmt) {
            block->statements.push_back(stmt);
        }
        
        if (hasError_) {
            return nullptr;
        }
        
        skipNewlines();
    }
    
    if (!match(TokenType::RBRACE)) {
        error("期望 '}'");
        return nullptr;
    }
    
    return block;
}

ASTPtr Parser::parseAssignment() {
    // 检查是否为环境变量赋值 %env.var=...
    if (current().type == TokenType::ENV_VAR) {
        std::string varName = current().value;
        advance();
        
        if (match(TokenType::ASSIGN)) {
            ASTPtr value = parseExpression();
            auto node = std::make_shared<EnvAssignmentNode>(varName, value);
            return node;
        }
        
        // 不是赋值，需要处理为表达式
        pos_--;
        return parseExpression();
    }
    
    // 检查是否为普通变量赋值 %var=...
    if (current().type == TokenType::VAR) {
        std::string varName = current().value;
        advance();
        
        if (match(TokenType::ASSIGN)) {
            ASTPtr value = parseExpression();
            auto node = std::make_shared<AssignmentNode>(varName, value);
            return node;
        }
        
        // 不是赋值，需要处理为表达式
        pos_--;
        return parseExpression();
    }
    
    return parseExpression();
}

ASTPtr Parser::parseExpression() {
    return parseComma();
}

ASTPtr Parser::parseComma() {
    ASTPtr left = parseTernary();
    
    while (check(TokenType::COMMA)) {
        advance();
        ASTPtr right = parseTernary();
        left = std::make_shared<BinaryOpNode>(",", left, right);
    }
    
    return left;
}

ASTPtr Parser::parseTernary() {
    ASTPtr expr = parseRange();
    
    if (check(TokenType::QUESTION)) {
        advance(); // 跳过 ?
        ASTPtr trueExpr = parseTernary(); // 允许嵌套三目
        if (!match(TokenType::COLON)) {
            error("期望 ':'");
            return nullptr;
        }
        ASTPtr falseExpr = parseTernary();
        return std::make_shared<TernaryOpNode>(expr, trueExpr, falseExpr);
    }
    
    return expr;
}

ASTPtr Parser::parseRange() {
    ASTPtr left = parseLogicalOr();
    
    while (check(TokenType::DOT) && pos_ + 1 < tokens_.size() && 
           (tokens_[pos_ + 1].type == TokenType::DOT)) {
        // 处理 .. 或 ..<
        advance(); // 跳过第一个 .
        advance(); // 跳过第二个 .
        bool exclusive = check(TokenType::LESS);
        if (exclusive) advance(); // 跳过 <
        
        ASTPtr right = parseLogicalOr();
        std::string op = exclusive ? "..<" : "..";
        left = std::make_shared<BinaryOpNode>(op, left, right);
        
        // 检查是否有步长 a..b..s
        if (!exclusive && check(TokenType::DOT) && pos_ + 1 < tokens_.size() && 
            tokens_[pos_ + 1].type == TokenType::DOT) {
            advance(); // 跳过第一个 .
            advance(); // 跳过第二个 .
            ASTPtr step = parseLogicalOr();
            // 用特殊标记存储步长：创建一个三元组 BinaryOpNode
            // op="..", left=之前的范围, right=步长
            left = std::make_shared<BinaryOpNode>("..step", left, step);
        }
    }
    
    return left;
}

ASTPtr Parser::parseLogicalOr() {
    ASTPtr left = parseLogicalAnd();
    
    while (check(TokenType::OR)) {
        advance();
        ASTPtr right = parseLogicalAnd();
        left = std::make_shared<BinaryOpNode>("||", left, right);
    }
    
    return left;
}

ASTPtr Parser::parseLogicalAnd() {
    ASTPtr left = parseBitwiseOr();
    
    while (check(TokenType::AND)) {
        advance();
        ASTPtr right = parseBitwiseOr();
        left = std::make_shared<BinaryOpNode>("&&", left, right);
    }
    
    return left;
}

ASTPtr Parser::parseBitwiseOr() {
    ASTPtr left = parseBitwiseXor();
    
    while (check(TokenType::PIPE)) {
        advance();
        ASTPtr right = parseBitwiseXor();
        left = std::make_shared<BinaryOpNode>("|", left, right);
    }
    
    return left;
}

ASTPtr Parser::parseBitwiseXor() {
    ASTPtr left = parseBitwiseAnd();
    
    while (check(TokenType::CARET)) {
        advance();
        ASTPtr right = parseBitwiseAnd();
        left = std::make_shared<BinaryOpNode>("^", left, right);
    }
    
    return left;
}

ASTPtr Parser::parseBitwiseAnd() {
    ASTPtr left = parseEquality();
    
    while (check(TokenType::AMPERSAND)) {
        advance();
        ASTPtr right = parseEquality();
        left = std::make_shared<BinaryOpNode>("&", left, right);
    }
    
    return left;
}

ASTPtr Parser::parseEquality() {
    ASTPtr left = parseRelational();
    
    while (check(TokenType::EQUAL) || check(TokenType::NOT_EQUAL)) {
        std::string op = current().value;
        advance();
        ASTPtr right = parseRelational();
        left = std::make_shared<BinaryOpNode>(op, left, right);
    }
    
    return left;
}

ASTPtr Parser::parseRelational() {
    ASTPtr left = parseShift();
    
    while (check(TokenType::LESS) || check(TokenType::LESS_EQ) ||
           check(TokenType::GREATER) || check(TokenType::GREATER_EQ)) {
        std::string op = current().value;
        advance();
        ASTPtr right = parseShift();
        left = std::make_shared<BinaryOpNode>(op, left, right);
    }
    
    return left;
}

ASTPtr Parser::parseShift() {
    ASTPtr left = parseAdditive();
    
    while (check(TokenType::LSHIFT) || check(TokenType::RSHIFT)) {
        std::string op = current().value;
        advance();
        ASTPtr right = parseAdditive();
        left = std::make_shared<BinaryOpNode>(op, left, right);
    }
    
    return left;
}

ASTPtr Parser::parseAdditive() {
    ASTPtr left = parseMultiplicative();
    
    while (check(TokenType::PLUS) || check(TokenType::MINUS)) {
        std::string op = current().value;
        advance();
        ASTPtr right = parseMultiplicative();
        left = std::make_shared<BinaryOpNode>(op, left, right);
    }
    
    return left;
}

ASTPtr Parser::parseMultiplicative() {
    ASTPtr left = parseUnary();
    
    while (check(TokenType::STAR) || check(TokenType::SLASH) || check(TokenType::PERCENT)) {
        std::string op = current().value;
        advance();
        ASTPtr right = parseUnary();
        left = std::make_shared<BinaryOpNode>(op, left, right);
    }
    
    return left;
}

ASTPtr Parser::parseUnary() {
    // 一元运算符
    if (check(TokenType::MINUS) || check(TokenType::NOT) || check(TokenType::TILDE)) {
        std::string op = current().value;
        advance();
        ASTPtr operand = parseUnary();
        return std::make_shared<UnaryOpNode>(op, operand, true);
    }
    
    return parsePostfix();
}

ASTPtr Parser::parsePostfix() {
    ASTPtr expr = parsePrimary();
    
    // 函数调用：支持两种形式
    // 1. %func(args) - VAR token
    // 2. func(args) - IDENTIFIER token
    if (expr && check(TokenType::LPAREN)) {
        if (expr->type == NodeType::VARIABLE) {
            auto varNode = std::static_pointer_cast<VariableNode>(expr);
            return parseFunctionCall(varNode->name);
        }
        if (expr->type == NodeType::LITERAL && std::holds_alternative<std::string>(std::static_pointer_cast<LiteralNode>(expr)->value)) {
            std::string name = std::get<std::string>(std::static_pointer_cast<LiteralNode>(expr)->value);
            return parseFunctionCall(name);
        }
    }
    
    return expr;
}

ASTPtr Parser::parsePrimary() {
    Token& tok = current();
    
    // 数字字面量（区分整数和浮点）
    if (tok.type == TokenType::NUMBER) {
        advance();
        if (tok.value.find('.') != std::string::npos) {
            // 浮点数
            double value = std::stod(tok.value);
            return std::make_shared<LiteralNode>(makeDoubleValue(value), tok.line, tok.column);
        } else {
            // 整数
            int64_t value = std::stoll(tok.value);
            return std::make_shared<LiteralNode>(makeIntValue(value), tok.line, tok.column);
        }
    }
    
    // 字符串字面量
    if (tok.type == TokenType::STRING) {
        advance();
        return std::make_shared<LiteralNode>(makeStringValue(tok.value), tok.line, tok.column);
    }
    
    // Unicode 字符串字面量
    if (tok.type == TokenType::UNICODE_STRING) {
        advance();
        return std::make_shared<LiteralNode>(makeStringValue(tok.value), tok.line, tok.column);
    }
    
    // 标识符作为字符串字面量（函数参数中的裸标识符）
    if (tok.type == TokenType::IDENTIFIER) {
        advance();
        return std::make_shared<LiteralNode>(makeStringValue(tok.value), tok.line, tok.column);
    }
    
    // true/false
    if (tok.type == TokenType::TRUE_KW) {
        advance();
        return std::make_shared<LiteralNode>(makeNumberValue(1.0), tok.line, tok.column);
    }
    
    if (tok.type == TokenType::FALSE_KW) {
        advance();
        return std::make_shared<LiteralNode>(makeNumberValue(0.0), tok.line, tok.column);
    }
    
    // 变量
    if (tok.type == TokenType::VAR) {
        advance();
        return std::make_shared<VariableNode>(tok.value, tok.line, tok.column);
    }
    
    // 环境变量
    if (tok.type == TokenType::ENV_VAR) {
        advance();
        return std::make_shared<EnvVariableNode>(tok.value, tok.line, tok.column);
    }
    
    // %env 查看所有环境变量
    if (tok.type == TokenType::ENV_VIEW) {
        advance();
        // 这应该是一个特殊的内建函数调用
        std::vector<ASTPtr> emptyArgs;
        return std::make_shared<FunctionCallNode>("env", emptyArgs, tok.line, tok.column);
    }
    
    // 命令执行
    if (tok.type == TokenType::COMMAND) {
        advance();
        std::string cmd = tok.value;
        std::vector<ASTPtr> args;
        
        // 收集命令参数
        while (pos_ < tokens_.size() && 
               current().type != TokenType::NEWLINE_TOK &&
               current().type != TokenType::SEMICOLON &&
               current().type != TokenType::PIPE &&
               current().type != TokenType::REDIRECT &&
               current().type != TokenType::EOF_TOKEN) {
            args.push_back(parsePrimary());
        }
        
        return std::make_shared<CommandExecNode>(cmd, args, tok.line, tok.column);
    }
    
    // 带引号的命令
    if (tok.type == TokenType::CMD_STRING) {
        advance();
        std::vector<ASTPtr> emptyArgs;
        return std::make_shared<CommandExecNode>(tok.value, emptyArgs, tok.line, tok.column);
    }
    
    // 命令输出替换
    if (tok.type == TokenType::CMD_OUTPUT) {
        advance();
        return std::make_shared<CommandOutputNode>(tok.value, tok.line, tok.column);
    }
    
    // 重定向
    if (tok.type == TokenType::REDIRECT) {
        advance();
        // 解析重定向内容: @("in.txt", "out.txt", "err.txt")
        std::vector<ASTPtr> targets;
        std::string content = tok.value;
        
        // 简单的逗号分隔解析
        std::istringstream iss(content);
        std::string part;
        while (std::getline(iss, part, ',')) {
            // 去除空格
            size_t start = part.find_first_not_of(" \t");
            size_t end = part.find_last_not_of(" \t");
            if (start == std::string::npos) continue;
            std::string trimmed = part.substr(start, end - start + 1);
            
            // 创建字符串字面量节点
            targets.push_back(std::make_shared<LiteralNode>(
                makeStringValue(trimmed), tok.line, tok.column));
        }
        
        return std::make_shared<RedirectExprNode>(nullptr, targets, tok.line, tok.column);
    }
    
    // 括号表达式
    if (tok.type == TokenType::LPAREN) {
        advance();
        ASTPtr expr = parseExpression();
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
        return expr;
    }
    
    // calc(...) 表达式
    if (tok.type == TokenType::CALC) {
        advance();
        if (!match(TokenType::LPAREN)) {
            error("calc 后期望 '('");
            return nullptr;
        }
        ASTPtr expr = parseExpression();
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
        return expr;
    }
    
    // lambda 定义
    if (tok.type == TokenType::LPAREN) {
        // 检查是否为 lambda ():{...} 或 (x, y):{...}
        size_t savedPos = pos_;
        advance();  // 跳过 (
        
        // 检查参数列表
        std::vector<std::string> lambdaParams;
        bool hasParams = false;
        while (current().type != TokenType::RPAREN && 
               current().type != TokenType::EOF_TOKEN) {
            if (current().type == TokenType::IDENTIFIER) {
                lambdaParams.push_back(current().value);
                hasParams = true;
                advance();
            } else if (current().type == TokenType::VAR) {
                lambdaParams.push_back(current().value);
                hasParams = true;
                advance();
            } else {
                break;
            }
            if (check(TokenType::COMMA)) {
                advance();
            }
        }
        
        if (current().type == TokenType::RPAREN) {
            advance();  // 跳过 )
            if (current().type == TokenType::COLON) {
                advance();  // 跳过 :
                if (current().type == TokenType::LBRACE) {
                    // lambda 定义
                    ASTPtr body = parseBlock();
                    auto node = std::make_shared<LambdaDefNode>(body, tok.line, tok.column);
                    node->paramNames = lambdaParams;
                    return node;
                }
            }
        }
        
        pos_ = savedPos;
    }
    
    error("意外的 Token: " + tok.value);
    return nullptr;
}

ASTPtr Parser::parseIfStatement() {
    Token& ifTok = current();
    advance();  // 跳过 if
    
    // 解析条件
    ASTPtr condition;
    if (check(TokenType::LPAREN)) {
        advance();  // 跳过 (
        condition = parseExpression();
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
    } else {
        condition = parseExpression();
    }
    
    // 解析 then 块
    ASTPtr thenBlock;
    if (check(TokenType::LBRACE)) {
        thenBlock = parseBlock();
    } else {
        // 单行 if
        auto block = std::make_shared<BlockNode>();
        block->statements.push_back(parseStatement());
        thenBlock = block;
    }
    
    auto ifNode = std::make_shared<IfNode>(condition, thenBlock, ifTok.line, ifTok.column);
    
    // 解析 elif 块
    while (check(TokenType::ELIF)) {
        advance();  // 跳过 elif
        
        ASTPtr elifCondition;
        if (check(TokenType::LPAREN)) {
            advance();
            elifCondition = parseExpression();
            if (!match(TokenType::RPAREN)) {
                error("期望 ')'");
                return nullptr;
            }
        } else {
            elifCondition = parseExpression();
        }
        
        ASTPtr elifBlock;
        if (check(TokenType::LBRACE)) {
            elifBlock = parseBlock();
        } else {
            auto block = std::make_shared<BlockNode>();
            block->statements.push_back(parseStatement());
            elifBlock = block;
        }
        
        ifNode->elifBlocks.push_back({elifCondition, elifBlock});
    }
    
    // 解析 else 块
    if (check(TokenType::ELSE)) {
        advance();  // 跳过 else
        
        if (check(TokenType::LBRACE)) {
            ifNode->elseBlock = parseBlock();
        } else {
            auto block = std::make_shared<BlockNode>();
            block->statements.push_back(parseStatement());
            ifNode->elseBlock = block;
        }
    }
    
    return ifNode;
}

ASTPtr Parser::parseForStatement() {
    Token& forTok = current();
    advance();  // 跳过 for
    
    if (!match(TokenType::LPAREN)) {
        error("for 后期望 '('");
        return nullptr;
    }
    
    // 解析循环变量
    if (!check(TokenType::VAR)) {
        error("for 循环中期望变量名");
        return nullptr;
    }
    
    std::string varName = current().value;
    advance();
    
    // 跳过 in
    if (!match(TokenType::IN)) {
        error("for 循环中期望 'in'");
        return nullptr;
    }
    
    // 解析范围
    ASTPtr range = parseExpression();
    
    if (!match(TokenType::RPAREN)) {
        error("期望 ')'");
        return nullptr;
    }
    
    // 解析循环体
    ASTPtr body;
    if (check(TokenType::LBRACE)) {
        body = parseBlock();
    } else {
        auto block = std::make_shared<BlockNode>();
        block->statements.push_back(parseStatement());
        body = block;
    }
    
    return std::make_shared<ForNode>(varName, range, body, forTok.line, forTok.column);
}

ASTPtr Parser::parseWhileStatement() {
    Token& whileTok = current();
    advance();  // 跳过 while
    
    // 解析条件
    ASTPtr condition;
    if (check(TokenType::LPAREN)) {
        advance();  // 跳过 (
        condition = parseExpression();
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
    } else {
        condition = parseExpression();
    }
    
    // 解析循环体
    ASTPtr body;
    if (check(TokenType::LBRACE)) {
        body = parseBlock();
    } else {
        auto block = std::make_shared<BlockNode>();
        block->statements.push_back(parseStatement());
        body = block;
    }
    
    return std::make_shared<WhileNode>(condition, body, whileTok.line, whileTok.column);
}

ASTPtr Parser::parseFunctionDef() {
    Token& nameTok = current();
    std::string funcName = nameTok.value;
    advance();  // 跳过函数名
    
    // 检查是否有 =（覆盖形式 f=():{...}）
    if (check(TokenType::ASSIGN)) {
        advance();  // 跳过 =
    }
    
    // 解析参数列表 (a, b)
    std::vector<std::string> params;
    if (match(TokenType::LPAREN)) {
        while (!check(TokenType::RPAREN) && !check(TokenType::EOF_TOKEN)) {
            if (current().type == TokenType::IDENTIFIER) {
                params.push_back(current().value);
                advance();
            } else if (current().type == TokenType::VAR) {
                params.push_back(current().value);
                advance();
            } else {
                break;
            }
            if (check(TokenType::COMMA)) {
                advance();
            }
        }
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
    }
    
    if (!match(TokenType::COLON)) {
        error("期望 ':'");
        return nullptr;
    }
    
    ASTPtr body = parseBlock();
    
    auto node = std::make_shared<FunctionDefNode>(funcName, body, nameTok.line, nameTok.column);
    node->paramNames = params;
    return node;
}

ASTPtr Parser::parseReturnStatement() {
    Token& retTok = current();
    advance();  // 跳过 return
    
    ASTPtr value = nullptr;
    if (check(TokenType::LPAREN)) {
        advance();  // 跳过 (
        value = parseExpression();
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
    }
    
    return std::make_shared<ReturnNode>(value, retTok.line, retTok.column);
}

ASTPtr Parser::parseBreakStatement() {
    Token& breakTok = current();
    advance();  // 跳过 break
    
    if (check(TokenType::LPAREN)) {
        advance();  // 跳过 (
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
    }
    
    return std::make_shared<BreakNode>(breakTok.line, breakTok.column);
}

ASTPtr Parser::parseContinueStatement() {
    Token& contTok = current();
    advance();  // 跳过 continue
    
    if (check(TokenType::LPAREN)) {
        advance();  // 跳过 (
        if (!match(TokenType::RPAREN)) {
            error("期望 ')'");
            return nullptr;
        }
    }
    
    return std::make_shared<ContinueNode>(contTok.line, contTok.column);
}

ASTPtr Parser::parseFunctionCall(const std::string& name) {
    Token& parenTok = current();
    advance();  // 跳过 (
    
    std::vector<ASTPtr> args;
    
    if (!check(TokenType::RPAREN)) {
        args = parseArgumentList();
    }
    
    if (!match(TokenType::RPAREN)) {
        error("期望 ')'");
        return nullptr;
    }
    
    return std::make_shared<FunctionCallNode>(name, args, parenTok.line, parenTok.column);
}

std::vector<ASTPtr> Parser::parseArgumentList() {
    std::vector<ASTPtr> args;
    
    // 参数列表中逗号用于分隔参数，不能走 parseComma
    args.push_back(parseTernary());
    
    while (check(TokenType::COMMA)) {
        advance();  // 跳过 ,
        args.push_back(parseTernary());
    }
    
    return args;
}

Token& Parser::current() {
    if (pos_ >= tokens_.size()) {
        // 返回一个 EOF Token
        static Token eofToken(TokenType::EOF_TOKEN);
        return eofToken;
    }
    return tokens_[pos_];
}

Token& Parser::advance() {
    if (pos_ < tokens_.size()) {
        pos_++;
    }
    return current();
}

Token& Parser::peek() {
    if (pos_ + 1 >= tokens_.size()) {
        static Token eofToken(TokenType::EOF_TOKEN);
        return eofToken;
    }
    return tokens_[pos_ + 1];
}

bool Parser::check(TokenType type) {
    return current().type == type;
}

bool Parser::checkValue(const std::string& value) {
    return current().value == value;
}

bool Parser::match(TokenType type) {
    if (check(type)) {
        advance();
        return true;
    }
    return false;
}

bool Parser::matchValue(const std::string& value) {
    if (checkValue(value)) {
        advance();
        return true;
    }
    return false;
}

Token Parser::expect(TokenType type, const std::string& message) {
    if (check(type)) {
        return advance();
    }
    error(message);
    return current();
}

Token Parser::expectValue(const std::string& value, const std::string& message) {
    if (checkValue(value)) {
        return advance();
    }
    error(message);
    return current();
}

void Parser::error(const std::string& message) {
    Token& tok = current();
    error_ = filename_ + ":" + std::to_string(tok.line) + ":" + 
             std::to_string(tok.column) + ": 错误: " + message;
    hasError_ = true;
}

void Parser::skipNewlines() {
    while (pos_ < tokens_.size() && 
           (current().type == TokenType::NEWLINE_TOK || current().type == TokenType::SEMICOLON)) {
        advance();
    }
}

bool Parser::isStatementEnd() {
    return current().type == TokenType::NEWLINE_TOK ||
           current().type == TokenType::SEMICOLON ||
           current().type == TokenType::EOF_TOKEN;
}

} // namespace wash
