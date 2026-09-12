/**
 * @file types.h
 * @brief wash 类型定义
 * 
 * 定义 Token、AST 节点、值类型等基础类型。
 * 
 * @author wash
 * @date 2026-09-12
 */

#ifndef WASH_TYPES_H
#define WASH_TYPES_H

#include <string>
#include <vector>
#include <variant>
#include <memory>
#include <map>
#include <functional>

namespace wash {

/**
 * @brief Token 类型枚举
 */
enum class TokenType {
    // 基础类型
    NUMBER,         ///< 数字字面量
    STRING,         ///< 字符串字面量
    IDENTIFIER,     ///< 标识符
    
    // 变量相关
    VAR,            ///< %var 变量
    ENV_VAR,        ///< %env.var 环境变量
    ENV_VIEW,       ///< %env 查看所有环境变量
    
    // 命令相关
    COMMAND,        ///< $cmd 命令执行
    CMD_STRING,     ///< $"cmd" 带引号的命令
    CMD_OUTPUT,     ///< $(...) 命令输出替换
    REDIRECT,       ///< @(...) 重定向
    
    // 运算符
    PLUS,           ///< +
    MINUS,          ///< -
    STAR,           ///< *
    SLASH,          ///< /
    PERCENT,        ///< %
    CARET,          ///< ^
    AMPERSAND,      ///< &
    PIPE,           ///< |
    TILDE,          ///< ~
    
    // 比较运算符
    LESS,           ///< <
    LESS_EQ,        ///< <=
    GREATER,        ///< >
    GREATER_EQ,     ///< >=
    EQUAL,          ///< ==
    NOT_EQUAL,      ///< !=
    
    // 逻辑运算符
    AND,            ///< &&
    OR,             ///< ||
    NOT,            ///< !
    
    // 移位运算符
    LSHIFT,         ///< <<
    RSHIFT,         ///< >>
    
    // 赋值
    ASSIGN,         ///< =
    
    // 分隔符
    LPAREN,         ///< (
    RPAREN,         ///< )
    LBRACE,         ///< {
    RBRACE,         ///< }
    LBRACKET,       ///< [
    RBRACKET,       ///< ]
    SEMICOLON,      ///< ;
    COMMA,          ///< ,
    COLON,          ///< :
    DOT,            ///< .
    
    // 关键字
    IF,             ///< if
    ELIF,           ///< elif
    ELSE,           ///< else
    FOR,            ///< for
    WHILE,          ///< while
    IN,             ///< in
    RETURN,         ///< return
    BREAK,          ///< break
    CONTINUE,       ///< continue
    TRUE,           ///< true
    FALSE,          ///< false
    CALC,           ///< calc
    
    // 特殊
    NEWLINE,        ///< 换行
    EOF_TOKEN,      ///< 文件结束
    ERROR,          ///< 错误
    NONE            ///< 无类型
};

/**
 * @brief Token 结构体
 */
struct Token {
    TokenType type;         ///< Token 类型
    std::string value;      ///< Token 值
    size_t line;            ///< 行号
    size_t column;          ///< 列号
    
    /**
     * @brief 构造函数
     * @param t Token 类型
     * @param v Token 值
     * @param l 行号
     * @param c 列号
     */
    Token(TokenType t = TokenType::NONE, const std::string& v = "", 
         size_t l = 0, size_t c = 0)
        : type(t), value(v), line(l), column(c) {}
};

/**
 * @brief 值类型（string 或 number）
 */
using Value = std::variant<std::string, double>;

/**
 * @brief 将值转换为字符串
 * @param val 值
 * @return 字符串表示
 */
inline std::string valueToString(const Value& val) {
    if (std::holds_alternative<std::string>(val)) {
        return std::get<std::string>(val);
    } else {
        return std::to_string(std::get<double>(val));
    }
}

/**
 * @brief 将值转换为数字
 * @param val 值
 * @return 数字值，转换失败返回 0
 */
inline double valueToNumber(const Value& val) {
    if (std::holds_alternative<double>(val)) {
        return std::get<double>(val);
    } else {
        const std::string& s = std::get<std::string>(val);
        try {
            return std::stod(s);
        } catch (...) {
            return 0.0;
        }
    }
}

/**
 * @brief 创建字符串值
 * @param s 字符串
 * @return Value 对象
 */
inline Value makeStringValue(const std::string& s) {
    return Value(s);
}

/**
 * @brief 创建数字值
 * @param n 数字
 * @return Value 对象
 */
inline Value makeNumberValue(double n) {
    return Value(n);
}

/**
 * @brief 判断值是否为真
 * @param val 值
 * @return 真值
 */
inline bool isTruthy(const Value& val) {
    if (std::holds_alternative<std::string>(val)) {
        return !std::get<std::string>(val).empty();
    } else {
        return std::get<double>(val) != 0.0;
    }
}

/**
 * @brief AST 节点类型枚举
 */
enum class NodeType {
    // 表达式
    LITERAL,            ///< 字面量（数字、字符串）
    VARIABLE,           ///< 变量引用
    ENV_VARIABLE,       ///< 环境变量引用
    BINARY_OP,          ///< 二元运算
    UNARY_OP,           ///< 一元运算
    TERNARY_OP,         ///< 三元运算
    FUNCTION_CALL,      ///< 函数调用
    
    // 语句
    ASSIGNMENT,         ///< 赋值
    COMMAND_EXEC,       ///< 命令执行
    COMMAND_OUTPUT,     ///< 命令输出替换
    PIPE_EXPR,          ///< 管道表达式
    REDIRECT_EXPR,      ///< 重定向表达式
    
    // 控制流
    IF_STMT,            ///< if 语句
    FOR_STMT,           ///< for 循环
    WHILE_STMT,         ///< while 循环
    BREAK_STMT,         ///< break 语句
    CONTINUE_STMT,      ///< continue 语句
    RETURN_STMT,        ///< return 语句
    
    // 函数
    FUNCTION_DEF,       ///< 函数定义
    LAMBDA_DEF,         ///< lambda 定义
    
    // 程序
    PROGRAM,            ///< 程序（语句列表）
    BLOCK,              ///< 代码块
    EXPRESSION_STMT,    ///< 表达式语句
};

/**
 * @brief AST 节点基类
 */
struct ASTNode {
    NodeType type;              ///< 节点类型
    size_t line;                ///< 行号
    size_t column;              ///< 列号
    
    /**
     * @brief 构造函数
     * @param t 节点类型
     * @param l 行号
     * @param c 列号
     */
    ASTNode(NodeType t, size_t l = 0, size_t c = 0)
        : type(t), line(l), column(c) {}
    
    /**
     * @brief 虚析构函数
     */
    virtual ~ASTNode() = default;
};

/// AST 节点指针类型
using ASTPtr = std::shared_ptr<ASTNode>;

/**
 * @brief 字面量节点
 */
struct LiteralNode : public ASTNode {
    Value value;                ///< 字面量值
    
    /**
     * @brief 构造函数
     * @param val 字面量值
     * @param l 行号
     * @param c 列号
     */
    LiteralNode(const Value& val, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::LITERAL, l, c), value(val) {}
};

/**
 * @brief 变量引用节点
 */
struct VariableNode : public ASTNode {
    std::string name;           ///< 变量名
    
    /**
     * @brief 构造函数
     * @param n 变量名
     * @param l 行号
     * @param c 列号
     */
    VariableNode(const std::string& n, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::VARIABLE, l, c), name(n) {}
};

/**
 * @brief 环境变量引用节点
 */
struct EnvVariableNode : public ASTNode {
    std::string name;           ///< 环境变量名
    
    /**
     * @brief 构造函数
     * @param n 环境变量名
     * @param l 行号
     * @param c 列号
     */
    EnvVariableNode(const std::string& n, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::ENV_VARIABLE, l, c), name(n) {}
};

/**
 * @brief 二元运算节点
 */
struct BinaryOpNode : public ASTNode {
    std::string op;             ///< 运算符
    ASTPtr left;                ///< 左操作数
    ASTPtr right;               ///< 右操作数
    
    /**
     * @brief 构造函数
     * @param o 运算符
     * @param l 左操作数
     * @param r 右操作数
     * @param ln 行号
     * @param c 列号
     */
    BinaryOpNode(const std::string& o, ASTPtr l, ASTPtr r, 
                 size_t ln = 0, size_t c = 0)
        : ASTNode(NodeType::BINARY_OP, ln, c), op(o), left(l), right(r) {}
};

/**
 * @brief 一元运算节点
 */
struct UnaryOpNode : public ASTNode {
    std::string op;             ///< 运算符
    ASTPtr operand;             ///< 操作数
    bool prefix;                ///< 是否为前缀运算符
    
    /**
     * @brief 构造函数
     * @param o 运算符
     * @param operand 操作数
     * @param p 是否为前缀
     * @param l 行号
     * @param c 列号
     */
    UnaryOpNode(const std::string& o, ASTPtr opd, bool p = true,
                size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::UNARY_OP, l, c), op(o), operand(opd), prefix(p) {}
};

/**
 * @brief 三元运算节点
 */
struct TernaryOpNode : public ASTNode {
    ASTPtr condition;           ///< 条件
    ASTPtr trueExpr;            ///< 真值表达式
    ASTPtr falseExpr;           ///< 假值表达式
    
    /**
     * @brief 构造函数
     * @param cond 条件
     * @param t 真值表达式
     * @param f 假值表达式
     * @param l 行号
     * @param c 列号
     */
    TernaryOpNode(ASTPtr cond, ASTPtr t, ASTPtr f, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::TERNARY_OP, l, c), condition(cond), trueExpr(t), falseExpr(f) {}
};

/**
 * @brief 函数调用节点
 */
struct FunctionCallNode : public ASTNode {
    std::string name;           ///< 函数名
    std::vector<ASTPtr> args;   ///< 参数列表
    
    /**
     * @brief 构造函数
     * @param n 函数名
     * @param a 参数列表
     * @param l 行号
     * @param c 列号
     */
    FunctionCallNode(const std::string& n, const std::vector<ASTPtr>& a = {},
                     size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::FUNCTION_CALL, l, c), name(n), args(a) {}
};

/**
 * @brief 赋值节点
 */
struct AssignmentNode : public ASTNode {
    std::string varName;        ///< 变量名
    ASTPtr value;               ///< 赋值表达式
    
    /**
     * @brief 构造函数
     * @param vn 变量名
     * @param v 赋值表达式
     * @param l 行号
     * @param c 列号
     */
    AssignmentNode(const std::string& vn, ASTPtr v, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::ASSIGNMENT, l, c), varName(vn), value(v) {}
};

/**
 * @brief 环境变量赋值节点
 */
struct EnvAssignmentNode : public ASTNode {
    std::string varName;        ///< 环境变量名
    ASTPtr value;               ///< 赋值表达式
    
    /**
     * @brief 构造函数
     * @param vn 环境变量名
     * @param v 赋值表达式
     * @param l 行号
     * @param c 列号
     */
    EnvAssignmentNode(const std::string& vn, ASTPtr v, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::ASSIGNMENT, l, c), varName(vn), value(v) {}
};

/**
 * @brief 命令执行节点
 */
struct CommandExecNode : public ASTNode {
    std::string command;                    ///< 命令名
    std::vector<ASTPtr> args;               ///< 参数列表
    
    /**
     * @brief 构造函数
     * @param cmd 命令名
     * @param a 参数列表
     * @param l 行号
     * @param c 列号
     */
    CommandExecNode(const std::string& cmd, const std::vector<ASTPtr>& a = {},
                    size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::COMMAND_EXEC, l, c), command(cmd), args(a) {}
};

/**
 * @brief 命令输出替换节点
 */
struct CommandOutputNode : public ASTNode {
    std::string command;                    ///< 命令字符串
    
    /**
     * @brief 构造函数
     * @param cmd 命令字符串
     * @param l 行号
     * @param c 列号
     */
    CommandOutputNode(const std::string& cmd, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::COMMAND_OUTPUT, l, c), command(cmd) {}
};

/**
 * @brief 管道表达式节点
 */
struct PipeExprNode : public ASTNode {
    ASTPtr left;                ///< 左表达式
    ASTPtr right;               ///< 右表达式
    
    /**
     * @brief 构造函数
     * @param l 左表达式
     * @param r 右表达式
     * @param ln 行号
     * @param c 列号
     */
    PipeExprNode(ASTPtr l, ASTPtr r, size_t ln = 0, size_t c = 0)
        : ASTNode(NodeType::PIPE_EXPR, ln, c), left(l), right(r) {}
};

/**
 * @brief 重定向表达式节点
 */
struct RedirectExprNode : public ASTNode {
    ASTPtr command;                         ///< 命令表达式
    std::vector<ASTPtr> targets;            ///< 重定向目标
    
    /**
     * @brief 构造函数
     * @param cmd 命令表达式
     * @param t 重定向目标
     * @param l 行号
     * @param c 列号
     */
    RedirectExprNode(ASTPtr cmd, const std::vector<ASTPtr>& t = {},
                     size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::REDIRECT_EXPR, l, c), command(cmd), targets(t) {}
};

/**
 * @brief if 语句节点
 */
struct IfNode : public ASTNode {
    ASTPtr condition;                       ///< 条件
    ASTPtr thenBlock;                       ///< then 块
    std::vector<std::pair<ASTPtr, ASTPtr>> elifBlocks;  ///< elif 块
    ASTPtr elseBlock;                       ///< else 块
    
    /**
     * @brief 构造函数
     * @param cond 条件
     * @param then then 块
     * @param l 行号
     * @param c 列号
     */
    IfNode(ASTPtr cond, ASTPtr then, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::IF_STMT, l, c), condition(cond), thenBlock(then) {}
};

/**
 * @brief for 循环节点
 */
struct ForNode : public ASTNode {
    std::string varName;                    ///< 循环变量名
    ASTPtr range;                           ///< 范围表达式
    ASTPtr body;                            ///< 循环体
    
    /**
     * @brief 构造函数
     * @param vn 循环变量名
     * @param r 范围表达式
     * @param b 循环体
     * @param l 行号
     * @param c 列号
     */
    ForNode(const std::string& vn, ASTPtr r, ASTPtr b, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::FOR_STMT, l, c), varName(vn), range(r), body(b) {}
};

/**
 * @brief while 循环节点
 */
struct WhileNode : public ASTNode {
    ASTPtr condition;                       ///< 条件
    ASTPtr body;                            ///< 循环体
    
    /**
     * @brief 构造函数
     * @param cond 条件
     * @param b 循环体
     * @param l 行号
     * @param c 列号
     */
    WhileNode(ASTPtr cond, ASTPtr b, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::WHILE_STMT, l, c), condition(cond), body(b) {}
};

/**
 * @brief break 语句节点
 */
struct BreakNode : public ASTNode {
    /**
     * @brief 构造函数
     * @param l 行号
     * @param c 列号
     */
    BreakNode(size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::BREAK_STMT, l, c) {}
};

/**
 * @brief continue 语句节点
 */
struct ContinueNode : public ASTNode {
    /**
     * @brief 构造函数
     * @param l 行号
     * @param c 列号
     */
    ContinueNode(size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::CONTINUE_STMT, l, c) {}
};

/**
 * @brief return 语句节点
 */
struct ReturnNode : public ASTNode {
    ASTPtr value;                           ///< 返回值
    
    /**
     * @brief 构造函数
     * @param v 返回值
     * @param l 行号
     * @param c 列号
     */
    ReturnNode(ASTPtr v = nullptr, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::RETURN_STMT, l, c), value(v) {}
};

/**
 * @brief 函数定义节点
 */
struct FunctionDefNode : public ASTNode {
    std::string name;                       ///< 函数名
    ASTPtr body;                            ///< 函数体
    
    /**
     * @brief 构造函数
     * @param n 函数名
     * @param b 函数体
     * @param l 行号
     * @param c 列号
     */
    FunctionDefNode(const std::string& n, ASTPtr b, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::FUNCTION_DEF, l, c), name(n), body(b) {}
};

/**
 * @brief lambda 定义节点
 */
struct LambdaDefNode : public ASTNode {
    ASTPtr body;                            ///< 函数体
    
    /**
     * @brief 构造函数
     * @param b 函数体
     * @param l 行号
     * @param c 列号
     */
    LambdaDefNode(ASTPtr b, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::LAMBDA_DEF, l, c), body(b) {}
};

/**
 * @brief 程序节点（语句列表）
 */
struct ProgramNode : public ASTNode {
    std::vector<ASTPtr> statements;         ///< 语句列表
    
    /**
     * @brief 构造函数
     * @param l 行号
     * @param c 列号
     */
    ProgramNode(size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::PROGRAM, l, c) {}
};

/**
 * @brief 代码块节点
 */
struct BlockNode : public ASTNode {
    std::vector<ASTPtr> statements;         ///< 语句列表
    
    /**
     * @brief 构造函数
     * @param l 行号
     * @param c 列号
     */
    BlockNode(size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::BLOCK, l, c) {}
};

/**
 * @brief 表达式语句节点
 */
struct ExpressionStmtNode : public ASTNode {
    ASTPtr expression;                      ///< 表达式
    
    /**
     * @brief 构造函数
     * @param expr 表达式
     * @param l 行号
     * @param c 列号
     */
    ExpressionStmtNode(ASTPtr expr, size_t l = 0, size_t c = 0)
        : ASTNode(NodeType::EXPRESSION_STMT, l, c), expression(expr) {}
};

/**
 * @brief 执行结果类型
 */
enum class ExecResultType {
    NORMAL,         ///< 正常
    RETURN,         ///< return 语句
    BREAK,          ///< break 语句
    CONTINUE,       ///< continue 语句
};

/**
 * @brief 执行结果
 */
struct ExecResult {
    ExecResultType type;        ///< 结果类型
    Value value;                ///< 返回值（如果有的话）
    
    /**
     * @brief 构造函数
     * @param t 结果类型
     * @param v 返回值
     */
    ExecResult(ExecResultType t = ExecResultType::NORMAL, 
               const Value& v = Value(std::string()))
        : type(t), value(v) {}
};

/**
 * @brief 变量作用域
 */
class Scope {
public:
    /**
     * @brief 构造函数
     * @param parent 父作用域
     */
    Scope(std::shared_ptr<Scope> parent = nullptr) : parent_(parent) {}
    
    /**
     * @brief 设置变量
     * @param name 变量名
     * @param value 变量值
     */
    void set(const std::string& name, const Value& value) {
        variables_[name] = value;
    }
    
    /**
     * @brief 获取变量
     * @param name 变量名
     * @param found 是否找到
     * @return 变量值
     */
    Value get(const std::string& name, bool& found) const {
        auto it = variables_.find(name);
        if (it != variables_.end()) {
            found = true;
            return it->second;
        }
        if (parent_) {
            return parent_->get(name, found);
        }
        found = false;
        return Value(std::string());
    }
    
    /**
     * @brief 检查变量是否存在
     * @param name 变量名
     * @return 是否存在
     */
    bool has(const std::string& name) const {
        if (variables_.find(name) != variables_.end()) {
            return true;
        }
        return parent_ ? parent_->has(name) : false;
    }
    
private:
    std::map<std::string, Value> variables_;    ///< 变量表
    std::shared_ptr<Scope> parent_;             ///< 父作用域
};

} // namespace wash

#endif // WASH_TYPES_H
