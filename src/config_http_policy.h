/**
 * @file src/config_http_policy.h
 * @brief Pure decisions used by the configuration HTTP API.
 */
#pragma once

#include <cstdint>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <vector>

namespace confighttp::policy {
  inline constexpr int https_port_offset = 1;

  enum class TokenScope {
    Read,
    Write,
  };

  bool is_token_route_eligible(std::string_view path);
  std::vector<std::string> ordered_methods_for_catalog(const std::set<std::string, std::less<>> &methods);
  std::string get_web_ui_host_for_local_open(std::string_view bind_address, bool prefer_ipv4_loopback);
  std::string make_web_ui_url(std::string_view host, std::uint16_t port, std::string_view path = {});
  /**
   * @brief Redirect target for an address of a former web UI page.
   * @param path Request path without the query string.
   * @param query Raw query string, carried over to the target.
   * @return Same-origin target, or std::nullopt if the path needs no redirect.
   */
  std::optional<std::string> web_ui_redirect(std::string_view path, std::string_view query);
  TokenScope scope_from_string(std::string_view scope);
  std::string scope_to_string(TokenScope scope);
}  // namespace confighttp::policy
