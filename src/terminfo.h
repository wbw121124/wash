/**
 * @file terminfo.h
 * @brief 最小 terminfo 二进制解析器
 * 
 * 自包含实现，不依赖 ncurses，避免与 readline 冲突。
 * 解析 /usr/share/terminfo/ 下的二进制文件，提取终端能力。
 * 
 * @author wash
 * @date 2026-09-14
 */

#ifndef WASH_TERMINFO_H
#define WASH_TERMINFO_H

#include <string>
#include <vector>
#include <cstdint>

namespace wash {

/**
 * @brief terminfo 能力信息
 */
struct TermInfoCapabilities {
    int colors;             ///< 颜色数（-1 表示未定义）
    std::string setaf;      ///< 设置前景色模板（如 "\E[3%dm"）
    std::string setab;      ///< 设置背景色模板（如 "\E[4%dm"）
    std::string sgr0;       ///< 重置所有属性
    std::string bold;       ///< 粗体模式
    std::string smul;       ///< 进入下划线
    std::string rmul;       ///< 退出下划线
    std::string sitm;       ///< 进入斜体
    std::string ritm;       ///< 退出斜体
    bool valid;             ///< 解析是否成功
    
    TermInfoCapabilities() : colors(-1), valid(false) {}
};

/**
 * @brief terminfo 二进制解析器
 * 
 * 从标准路径读取 terminfo 二进制文件并解析。
 * 不调用任何 ncurses API。
 */
class TerminfoParser {
public:
    /**
     * @brief 解析当前终端的 terminfo
     * @param term 环境变量 TERM 的值（为空则自动读取）
     * @return 解析后的能力信息
     */
    static TermInfoCapabilities parse(const std::string& term = "");

    /**
     * @brief 将 terminfo 字符串模板中的 \E 等转义转换为实际字节
     * @param tiStr terminfo 字符串
     * @return 转换后的字符串
     */
    static std::string unescape(const std::string& tiStr);

private:
    static const int MAGIC = 0432;  ///< terminfo 魔数（八进制）
    
    static std::string findTerminfoFile(const std::string& term);
    static std::vector<std::string> getSearchPaths();
    static std::string getTerminfoDir();
    static std::string getTerminfoDirs();
};

} // namespace wash

#endif // WASH_TERMINFO_H
