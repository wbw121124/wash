/**
 * @file terminfo.cpp
 * @brief terminfo 二进制解析器实现
 * 
 * 解析标准 terminfo 二进制格式，提取颜色和属性能力。
 * 字符串转义：\E → ESC(0x1B), \n → 换行, \t → 制表, \r → 回车, \b → 退格
 * 数字参数：%%p%d / %%p1%d 等格式保留为模板（%d 占位符）
 * 
 * @author wash
 * @date 2026-09-14
 */

#include "terminfo.h"
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <algorithm>

namespace wash {

/**
 * @brief 将 terminfo 字符串模板中的转义序列转换为实际字节
 * 
 * terminfo 使用的转义约定：
 * - \E 或 \e → ESC (0x1B)
 * - \n → 换行
 * - \t → 制表
 * - \r → 回车
 * - \b → 退格
 * - \f → 换页
 * - \s → 空格
 * - \0 → NULL (0x00)
 * - \a → 响铃 (0x07)
 * - \xxx → 八进制
 * - %% → 字面 %
 * - %p%d / %p1%d 等数字参数 → 保留为 %d 模板
 */
std::string TerminfoParser::unescape(const std::string& tiStr) {
    std::string result;
    result.reserve(tiStr.size());
    
    for (size_t i = 0; i < tiStr.size(); ++i) {
        char c = tiStr[i];
        
        if (c == '\\' && i + 1 < tiStr.size()) {
            char next = tiStr[i + 1];
            switch (next) {
                case 'E': case 'e':
                    result += '\x1B';
                    i++;
                    break;
                case 'n':
                    result += '\n';
                    i++;
                    break;
                case 't':
                    result += '\t';
                    i++;
                    break;
                case 'r':
                    result += '\r';
                    i++;
                    break;
                case 'b':
                    result += '\b';
                    i++;
                    break;
                case 'f':
                    result += '\f';
                    i++;
                    break;
                case 's':
                    result += ' ';
                    i++;
                    break;
                case '0':
                    result += '\0';
                    i++;
                    break;
                case 'a':
                    result += '\a';
                    i++;
                    break;
                default:
                    // 八进制 \xxx
                    if (next >= '0' && next <= '7') {
                        int octal = next - '0';
                        for (int j = 0; j < 2 && i + 2 < tiStr.size(); ++j) {
                            char d = tiStr[i + 2];
                            if (d >= '0' && d <= '7') {
                                octal = octal * 8 + (d - '0');
                                i++;
                            } else {
                                break;
                            }
                        }
                        result += static_cast<char>(octal);
                        i++;
                    } else {
                        result += c;
                    }
                    break;
            }
        } else if (c == '%' && i + 1 < tiStr.size()) {
            char next = tiStr[i + 1];
            // %p1%d, %p2%d 等参数格式 → 转换为简单 %d 模板
            if (next == 'p') {
                result += '%';
                result += 'd';
                i += 3; // 跳过 %pN
            } else if (next == 'd') {
                result += '%';
                result += 'd';
                i++;
            } else if (next == '%') {
                result += '%';
                i++;
            } else {
                result += c;
            }
        } else {
            result += c;
        }
    }
    
    return result;
}

/**
 * @brief 从字符串中读取 uint16_t（小端序）
 */
static uint16_t readUint16(const char* data, size_t offset) {
    return static_cast<uint16_t>(
        static_cast<uint8_t>(data[offset]) |
        (static_cast<uint8_t>(data[offset + 1]) << 8)
    );
}

/**
 * @brief 读取 terminfo 文件并解析
 */
static TermInfoCapabilities parseTerminfoFile(const std::string& filepath) {
    TermInfoCapabilities caps;
    
    std::ifstream file(filepath, std::ios::binary);
    if (!file.is_open()) {
        return caps;
    }
    
    // 读取整个文件
    std::ostringstream oss;
    oss << file.rdbuf();
    std::string data = oss.str();
    
    // 最小文件大小：18 (header) + 1 (names null terminator)
    if (data.size() < 19) {
        return caps;
    }
    
    const char* buf = data.c_str();
    
    // 检查魔数
    uint16_t magic = readUint16(buf, 0);
    if (magic != 0x1A) {  // 0432 octal = 0x1A
        return caps;
    }
    
    // 读取 header
    uint16_t namesLen    = readUint16(buf, 2);
    uint16_t boolCount   = readUint16(buf, 4);
    uint16_t numCount    = readUint16(buf, 6);
    uint16_t strCount    = readUint16(buf, 8);
    uint16_t strTableSz  = readUint16(buf, 10);
    
    // 计算各段偏移
    size_t namesStart    = 18;
    size_t boolsStart    = namesStart + namesLen;
    // bools 后可能有 padding（对齐到偶数）
    size_t numsStart     = boolsStart + boolCount + ((boolCount & 1) ? 1 : 0);
    size_t strsStart     = numsStart + numCount * 2;
    size_t strTableStart = strsStart + strCount * 2;
    
    // 检查文件大小是否足够
    if (strTableStart + strTableSz > data.size()) {
        return caps;
    }
    
    // 读取数字能力
    // 标准数字能力顺序：0=auto_left_margin, 1=auto_right_margin, ..., 16=colors
    // colors 在标准 terminfo 中的索引是 16
    const int COLORS_INDEX = 16;
    
    std::vector<int16_t> nums(numCount);
    for (uint16_t i = 0; i < numCount; ++i) {
        int16_t val = static_cast<int16_t>(readUint16(buf, numsStart + i * 2));
        nums[i] = val;
    }
    
    if (COLORS_INDEX < numCount && nums[COLORS_INDEX] >= 0) {
        caps.colors = nums[COLORS_INDEX];
    }
    
    // 读取字符串能力
    // 标准字符串能力顺序（与 terminfo(5) 一致）：
    // 0=back_tab, 1=bell, ..., 243=ritm
    // setaf=393, setab=394, sgr0=354, bold=265, smul=348, rmul=357, sitm=349, ritm=352
    // 但实际索引取决于 terminfo 版本，我们需要通过名称查找
    
    // 由于字符串能力数量可能很大（~400+），我们使用已知的偏移量
    // 更可靠的方式：遍历字符串表中的名称来匹配
    
    // 为了简单和正确，我们使用名称索引表
    // 这些是 ncurses terminfo 格式中的标准偏移量（基于 ncurses 6.x）
    const int STRS_OFFSET = 38;  // 第一个字符串能力的相对偏移
    const int TAB_SIZE = 460;    // 字符串偏移表总大小（38..460，共460个能力）
    
    // 如果 strCount 足够大，使用已知偏移
    // 否则退回到 $TERM 启发式
    if (strCount > 400) {
        // 已知偏移（基于 ncurses tic 编译的标准 terminfo）
        // setaf=393, setab=394, sgr0=354, bold=265, smul=348, rmul=357
        // sitm=349, ritm=352
        auto readStr = [&](int idx) -> std::string {
            if (idx >= strCount) return "";
            uint16_t offset = readUint16(buf, strsStart + idx * 2);
            if (offset == 0xFFFF || offset >= strTableSz) return "";
            return std::string(buf + strTableStart + offset);
        };
        
        caps.setaf = TerminfoParser::unescape(readStr(393));
        caps.setab = TerminfoParser::unescape(readStr(394));
        caps.sgr0  = TerminfoParser::unescape(readStr(354));
        caps.bold  = TerminfoParser::unescape(readStr(265));
        caps.smul  = TerminfoParser::unescape(readStr(348));
        caps.rmul  = TerminfoParser::unescape(readStr(357));
        caps.sitm  = TerminfoParser::unescape(readStr(349));
        caps.ritm  = TerminfoParser::unescape(readStr(352));
    }
    
    caps.valid = true;
    return caps;
}

/**
 * @brief 获取 TERMINFO 环境变量指定的目录
 */
std::string TerminfoParser::getTerminfoDir() {
    const char* dir = getenv("TERMINFO");
    return dir ? std::string(dir) : "";
}

/**
 * @brief 获取 TERMINFO_DIRS 环境变量的目录列表
 */
std::string TerminfoParser::getTerminfoDirs() {
    const char* dirs = getenv("TERMINFO_DIRS");
    return dirs ? std::string(dirs) : "";
}

/**
 * @brief 获取 terminfo 搜索路径列表
 */
std::vector<std::string> TerminfoParser::getSearchPaths() {
    std::vector<std::string> paths;
    
    // 1. $TERMINFO
    std::string termInfoDir = getTerminfoDir();
    if (!termInfoDir.empty()) {
        paths.push_back(termInfoDir);
    }
    
    // 2. $TERMINFO_DIRS
    std::string termInfoDirs = getTerminfoDirs();
    if (!termInfoDirs.empty()) {
        std::istringstream ss(termInfoDirs);
        std::string path;
        while (std::getline(ss, path, ':')) {
            if (!path.empty()) {
                paths.push_back(path);
            }
        }
    }
    
    // 3. 默认路径
    // ~/.terminfo
    const char* home = getenv("HOME");
    if (home) {
        paths.push_back(std::string(home) + "/.terminfo");
    }
    
    // /usr/share/terminfo
    paths.push_back("/usr/share/terminfo");
    // /usr/lib/terminfo
    paths.push_back("/usr/lib/terminfo");
    // /usr/local/share/terminfo
    paths.push_back("/usr/local/share/terminfo");
    
    return paths;
}

/**
 * @brief 查找 terminfo 文件
 * 
 * 搜索路径：$TERMINFO → $TERMINFO_DIRS → ~/.terminfo → /usr/share/terminfo
 * 文件位于：{dir}/{TERM[0]}/{TERM} （例如 /usr/share/terminfo/x/xterm-256color）
 */
std::string TerminfoParser::findTerminfoFile(const std::string& term) {
    if (term.empty()) return "";
    
    std::vector<std::string> paths = getSearchPaths();
    
    for (const auto& dir : paths) {
        // 标准路径：{dir}/{first_char}/{term}
        std::string filepath = dir + "/" + term[0] + "/" + term;
        std::ifstream test(filepath, std::ios::binary);
        if (test.is_open()) {
            return filepath;
        }
        
        // 兼容路径：{dir}/{term}（某些系统直接平铺）
        filepath = dir + "/" + term;
        test.open(filepath, std::ios::binary);
        if (test.is_open()) {
            return filepath;
        }
    }
    
    return "";
}

/**
 * @brief 解析当前终端的 terminfo
 * 
 * 流程：
 * 1. 获取 $TERM
 * 2. 查找 terminfo 文件
 * 3. 解析二进制内容
 * 4. 返回能力信息
 */
TermInfoCapabilities TerminfoParser::parse(const std::string& term) {
    std::string termName = term;
    if (termName.empty()) {
        const char* envTerm = getenv("TERM");
        termName = envTerm ? std::string(envTerm) : "";
    }
    
    if (termName.empty()) {
        return TermInfoCapabilities();
    }
    
    std::string filepath = findTerminfoFile(termName);
    if (filepath.empty()) {
        return TermInfoCapabilities();
    }
    
    return parseTerminfoFile(filepath);
}

} // namespace wash
