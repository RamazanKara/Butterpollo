/**
 * @file src/boost_process_shim.h
 * @brief Minimal compatibility layer that maps our Boost.Process usage onto v2.
 */
#pragma once

  #include <WinSock2.h>

#ifndef BOOST_PROCESS_V2_HEADER_ONLY
  #define BOOST_PROCESS_V2_HEADER_ONLY
#endif
#include <boost/algorithm/string/predicate.hpp>
#include <boost/asio/system_executor.hpp>
#include <boost/process/v2/environment.hpp>
#include <boost/process/v2/process.hpp>
#include <algorithm>
#include <boost/filesystem/path.hpp>
#include <boost/system/error_code.hpp>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <system_error>
#include <type_traits>
#include <vector>

  #include <processthreadsapi.h>
  #include <Windows.h>

namespace boost_process_shim {

  namespace v2 = boost::process::v2;
  using process_environment_t = v2::process_environment;

  using pid_t = v2::pid_type;

  namespace detail {
    inline std::string to_utf8(const std::wstring &input) {
      if (input.empty()) {
        return {};
      }
      int required = WideCharToMultiByte(CP_UTF8, 0, input.data(), (int) input.size(), nullptr, 0, nullptr, nullptr);
      if (required <= 0) {
        return {};
      }
      std::string output;
      output.resize((size_t) required);
      WideCharToMultiByte(CP_UTF8, 0, input.data(), (int) input.size(), output.data(), required, nullptr, nullptr);
      return output;
    }

    inline std::wstring from_utf8(const std::string &input) {
      if (input.empty()) {
        return {};
      }
      int required = MultiByteToWideChar(CP_UTF8, 0, input.data(), (int) input.size(), nullptr, 0);
      if (required <= 0) {
        return {};
      }
      std::wstring output;
      output.resize((size_t) required);
      MultiByteToWideChar(CP_UTF8, 0, input.data(), (int) input.size(), output.data(), required);
      return output;
    }

    template<typename Char>
    inline bool names_equal(const std::basic_string<Char> &lhs, const std::basic_string<Char> &rhs) {
      return boost::iequals(lhs, rhs);
    }
  }  // namespace detail

  template<typename Char>
  struct env_entry {
    std::basic_string<Char> name;
    std::basic_string<Char> value;

    const std::basic_string<Char> &get_name() const {
      return name;
    }

    std::basic_string<Char> to_string() const {
      return value;
    }

    void assign(const std::basic_string<Char> &val) {
      value = val;
    }

    void clear() {
      value.clear();
    }
  };

  template<typename Char>
  class env_value_ref {
  public:
    explicit env_value_ref(env_entry<Char> &entry):
        _entry(entry) {
    }

    env_value_ref &operator=(const std::basic_string<Char> &value) {
      _entry.value = value;
      return *this;
    }

    env_value_ref &operator=(const Char *value) {
      if (value) {
        _entry.value = value;
      } else {
        _entry.value.clear();
      }
      return *this;
    }

    void assign(const std::basic_string<Char> &value) {
      _entry.assign(value);
    }

    void clear() {
      _entry.clear();
    }

    std::basic_string<Char> to_string() const {
      return _entry.to_string();
    }

    const std::basic_string<Char> &get_name() const {
      return _entry.get_name();
    }

  private:
    env_entry<Char> &_entry;
  };

  template<typename Char>
  class basic_environment {
  public:
    using entry = env_entry<Char>;
    using iterator = typename std::vector<entry>::iterator;
    using const_iterator = typename std::vector<entry>::const_iterator;

    basic_environment() = default;

    static basic_environment current() {
      basic_environment env;
      if constexpr (std::is_same_v<Char, wchar_t>) {
        for (auto kvp : v2::environment::current()) {
          env._entries.push_back(entry {kvp.key().wstring(), kvp.value().wstring()});
        }
      } else {
        for (auto kvp : v2::environment::current()) {
          auto name = kvp.key().wstring();
          auto value = kvp.value().wstring();
          env._entries.push_back(entry {detail::to_utf8(name), detail::to_utf8(value)});
        }
      }
      return env;
    }

    env_value_ref<Char> operator[](const std::basic_string<Char> &name) {
      return env_value_ref<Char>(find_or_create(name));
    }

    env_value_ref<Char> operator[](const Char *name) {
      return (*this)[std::basic_string<Char>(name ? name : std::basic_string<Char>())];
    }

    iterator begin() {
      return _entries.begin();
    }

    iterator end() {
      return _entries.end();
    }

    const_iterator begin() const {
      return _entries.begin();
    }

    const_iterator end() const {
      return _entries.end();
    }

    const_iterator cbegin() const {
      return _entries.cbegin();
    }

    const_iterator cend() const {
      return _entries.cend();
    }

    process_environment_t to_process_environment() const {
      if constexpr (std::is_same_v<Char, wchar_t>) {
        std::vector<std::wstring> env_buffer;
        env_buffer.reserve(_entries.size());
        for (const auto &entry : _entries) {
          env_buffer.push_back(entry.get_name() + L"=" + entry.to_string());
        }
        return process_environment_t(env_buffer);
      } else {
        std::vector<std::wstring> env_buffer;
        env_buffer.reserve(_entries.size());
        for (const auto &entry : _entries) {
          env_buffer.push_back(detail::from_utf8(entry.get_name()) + L"=" + detail::from_utf8(entry.to_string()));
        }
        return process_environment_t(env_buffer);
      }
    }

  private:
    entry *find_entry(const std::basic_string<Char> &name) {
      auto it = std::find_if(_entries.begin(), _entries.end(), [&](const auto &item) {
        return detail::names_equal(item.name, name);
      });
      if (it == _entries.end()) {
        return nullptr;
      }
      return &(*it);
    }

    entry &find_or_create(const std::basic_string<Char> &name) {
      if (auto *found = find_entry(name)) {
        return *found;
      }
      _entries.push_back(entry {name, {}});
      return _entries.back();
    }

    std::vector<entry> _entries;
  };

  using environment = basic_environment<char>;
  using native_environment = environment;
  using wenvironment = basic_environment<wchar_t>;

  class child {
  public:
    child() = default;

    explicit child(v2::process proc):
        _proc(std::move(proc)) {
    }

    explicit child(pid_t pid) {
      _proc.emplace(boost::asio::system_executor(), pid);
    }

    child(child &&) noexcept = default;
    child &operator=(child &&) noexcept = default;
    child(const child &) = delete;
    child &operator=(const child &) = delete;

    bool running() {
      if (!_proc) {
        return false;
      }
      return _proc->running();
    }

    bool running(std::error_code &ec) {
      if (!_proc) {
        ec = std::make_error_code(std::errc::invalid_argument);
        return false;
      }
      boost::system::error_code bec;
      auto res = _proc->running(bec);
      ec = std::error_code(bec.value(), bec.category());
      return res;
    }

    int wait(std::error_code &ec) {
      if (!_proc) {
        ec = std::make_error_code(std::errc::invalid_argument);
        return -1;
      }
      boost::system::error_code bec;
      auto res = _proc->wait(bec);
      ec = std::error_code(bec.value(), bec.category());
      return res;
    }

    int wait() {
      if (_proc) {
        return _proc->wait();
      }
      return 0;
    }

    int exit_code() const {
      if (!_proc) {
        return 0;
      }
      return _proc->exit_code();
    }

    v2::native_exit_code_type native_exit_code() const {
      if (!_proc) {
        return {};
      }
      return _proc->native_exit_code();
    }

    pid_t id() const {
      if (!_proc) {
        return 0;
      }
      return _proc->id();
    }

    void detach() {
      if (_proc) {
        _proc->detach();
        _proc.reset();
      }
    }

    bool valid() const {
      return _proc.has_value() && _proc->is_open();
    }

    explicit operator bool() const {
      return valid();
    }

  private:
    std::optional<v2::process> _proc;
  };

  class group {
  public:
    group() {
      job_ = CreateJobObjectW(nullptr, nullptr);
      if (job_) {
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION info {};
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(job_, JobObjectExtendedLimitInformation, &info, sizeof(info));
      }
    }

    ~group() {
      detach();
    }

    group(group &&other) noexcept {
      job_ = other.job_;
      other.job_ = nullptr;
    }

    group &operator=(group &&other) noexcept {
      if (this == &other) {
        return *this;
      }
      detach();
      job_ = other.job_;
      other.job_ = nullptr;
      return *this;
    }

    group(const group &) = delete;
    group &operator=(const group &) = delete;

    bool valid() const {
      return job_ != nullptr;
    }

    explicit operator bool() const {
      return valid();
    }

    using native_handle_type = HANDLE;

    native_handle_type native_handle() const {
      return job_;
    }

    void detach() {
      if (job_) {
        CloseHandle(job_);
        job_ = nullptr;
      }
    }

    void terminate(std::error_code &ec) {
      ec.clear();
      if (!job_) {
        return;
      }
      if (!TerminateJobObject(job_, 1)) {
        ec = std::error_code((int) GetLastError(), std::system_category());
      }
    }


  private:
    HANDLE job_ {nullptr};
  };

  namespace detail {
  }  // namespace detail

  namespace this_process {
    inline environment env() {
      return environment::current();
    }

    inline wenvironment wenv() {
      return wenvironment::current();
    }
  }  // namespace this_process

  inline std::filesystem::path search_path(const std::string &filename) {
    auto p = v2::environment::find_executable(filename);
    return std::filesystem::path(p.string());
  }

}  // namespace boost_process_shim
