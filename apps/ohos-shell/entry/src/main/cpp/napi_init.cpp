#include "napi/native_api.h"
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

extern "C" {
uint16_t lb_protocol_version(void);
uint32_t lb_features_bits(void);
char *lb_features_label(void);
char *lb_make_pairing_code(void);
int32_t lb_make_identity(const char *alias, char **out_device_id, char **out_alias, char **out_fp_hex);
int32_t lb_self_loopback_transfer(uint64_t *out_bytes);
char *lb_last_error(void);
void lb_string_free(char *s);
void lb_bytes_free(uint8_t *p, size_t len);

int32_t lb_host_start(const char *data_dir, const char *alias, uint16_t *out_port, char **out_code,
                      uint8_t *out_paired);
int32_t lb_host_await_pair(char **out_peer_id);
int32_t lb_host_stop(void);
int32_t lb_host_recv_file(const char *dest_root, uint64_t *out_bytes, char **out_path);
int32_t lb_host_recv_clipboard_text(char **out_text);
int32_t lb_host_recv_clipboard(uint8_t *out_kind, uint8_t **out_bytes, size_t *out_len);

int32_t lb_guest_open(const char *data_dir, const char *alias, uint8_t *out_paired, char **out_host,
                      uint16_t *out_port, char **out_peer_id);
int32_t lb_guest_pair(const char *host, uint16_t port, const char *code);
int32_t lb_guest_update_endpoint(const char *host, uint16_t port);
int32_t lb_guest_send_file(const char *file_path, const char *remote_name, uint64_t *out_bytes);
int32_t lb_guest_send_clipboard_text(const char *text);
int32_t lb_guest_send_clipboard_png(const uint8_t *data, size_t len);

int32_t lb_self_loopback_clipboard_text(const char *text, char **out_text);
int32_t lb_self_loopback_clipboard_png(const uint8_t *data, size_t len, uint8_t **out_bytes, size_t *out_len);

int32_t lb_session_open(const char *data_dir, const char *device_id, const char *alias, char **out_status_json);
int32_t lb_session_status(char **out_status_json);
int32_t lb_session_go_online(uint16_t port, uint16_t *out_port);
int32_t lb_session_go_offline(void);
int32_t lb_session_unpair(char **out_status_json);
int32_t lb_session_begin_pair(char **out_peer_id);
int32_t lb_session_pair(const char *host, uint16_t port, const char *code);
int32_t lb_session_set_peer(const char *host, uint16_t port);
int32_t lb_session_probe_peer(void);
int32_t lb_session_send_file(const char *file_path, const char *remote_name, uint64_t *out_bytes);
int32_t lb_session_send_clipboard_text(const char *text);
int32_t lb_session_send_clipboard_png(const uint8_t *data, size_t len);
int32_t lb_session_serve_once(const char *dest_root, char **out_event_json);
int32_t lb_session_pairing_code(char **out_code);
}

static bool ReadUtf8(napi_env env, napi_value v, char *buf, size_t buflen) {
    if (buflen == 0) {
        return false;
    }
    buf[0] = '\0';
    size_t len = 0;
    napi_status st = napi_get_value_string_utf8(env, v, buf, buflen - 1, &len);
    return st == napi_ok;
}

static void FillRcError(char *err, size_t errLen, const char *prefix, int32_t rc) {
    char *detail = lb_last_error();
    if (detail != nullptr && detail[0] != '\0') {
        snprintf(err, errLen, "%s failed rc=%d (%s)", prefix, rc, detail);
        lb_string_free(detail);
    } else {
        snprintf(err, errLen, "%s failed rc=%d", prefix, rc);
    }
}

static bool ReadUtf8Owned(napi_env env, napi_value v, std::string *out, size_t max_bytes) {
    size_t needed = 0;
    napi_status st = napi_get_value_string_utf8(env, v, nullptr, 0, &needed);
    if (st != napi_ok) {
        return false;
    }
    if (needed > max_bytes) {
        return false;
    }
    std::vector<char> buf(needed + 1, '\0');
    size_t written = 0;
    st = napi_get_value_string_utf8(env, v, buf.data(), buf.size(), &written);
    if (st != napi_ok) {
        out->clear();
        return false;
    }
    out->assign(buf.data(), written);
    return true;
}

static constexpr size_t kMaxClipTextBytes = 1024 * 1024;
static constexpr size_t kMaxClipPngBytes = 16 * 1024 * 1024;
static constexpr size_t kMaxPathBytes = 1024;

static void RejectWithMessage(napi_env env, napi_deferred deferred, const char *msg) {
    napi_value err = nullptr;
    napi_value text = nullptr;
    napi_create_string_utf8(env, msg != nullptr ? msg : "error", NAPI_AUTO_LENGTH, &text);
    napi_create_error(env, nullptr, text, &err);
    napi_reject_deferred(env, deferred, err);
}

enum class AsyncOp {
    SelfLoopbackTransfer,
    HostAwaitPair,
    HostRecvFile,
    GuestPair,
    GuestSendFile,
    SelfLoopbackClipboardText,
    HostRecvClipboardText,
    GuestSendClipboardText,
    HostRecvClipboard,
    GuestSendClipboardPng,
    SelfLoopbackClipboardPng,
    SessionGoOnline,
    SessionBeginPair,
    SessionPair,
    SessionSendFile,
    SessionSendClipboardText,
    SessionSendClipboardPng,
    SessionServeOnce,
    SessionStatus,
    SessionProbePeer,
    SessionUnpair,
};

struct AsyncWorkData {
    napi_async_work asyncWork = nullptr;
    napi_deferred deferred = nullptr;
    AsyncOp op = AsyncOp::SelfLoopbackTransfer;

    int32_t rc = 0;
    char err[512] = {0};

    std::string destRoot;
    std::string host;
    std::string code;
    std::string filePath;
    std::string remoteName;
    std::string text;
    std::vector<uint8_t> pngIn;
    uint16_t port = 0;

    uint64_t bytesOut = 0;
    std::string pathOut;
    std::string textOut;
    uint8_t clipKind = 0;
    std::vector<uint8_t> bytesOutBuf;
    std::string jsonOut;
    uint16_t listenPort = 0;
};

static void AsyncExecute(napi_env /*env*/, void *data) {
    auto *job = reinterpret_cast<AsyncWorkData *>(data);
    switch (job->op) {
        case AsyncOp::SelfLoopbackTransfer: {
            uint64_t n = 0;
            job->rc = lb_self_loopback_transfer(&n);
            job->bytesOut = n;
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_self_loopback_transfer", job->rc);
            }
            break;
        }
        case AsyncOp::HostAwaitPair: {
            char *peer = nullptr;
            job->rc = lb_host_await_pair(&peer);
            if (job->rc != 0) {
                if (peer) {
                    lb_string_free(peer);
                }
                FillRcError(job->err, sizeof(job->err), "lb_host_await_pair", job->rc);
            } else {
                job->textOut = peer ? peer : "";
                if (peer) {
                    lb_string_free(peer);
                }
            }
            break;
        }
        case AsyncOp::HostRecvFile: {
            uint64_t n = 0;
            char *path = nullptr;
            job->rc = lb_host_recv_file(job->destRoot.c_str(), &n, &path);
            job->bytesOut = n;
            if (job->rc != 0 || path == nullptr) {
                if (path) {
                    lb_string_free(path);
                }
                FillRcError(job->err, sizeof(job->err), "lb_host_recv_file", job->rc);
            } else {
                job->pathOut = path;
                lb_string_free(path);
            }
            break;
        }
        case AsyncOp::GuestPair: {
            job->rc = lb_guest_pair(job->host.c_str(), job->port, job->code.c_str());
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_guest_pair", job->rc);
            }
            break;
        }
        case AsyncOp::GuestSendFile: {
            uint64_t n = 0;
            job->rc = lb_guest_send_file(job->filePath.c_str(), job->remoteName.c_str(), &n);
            job->bytesOut = n;
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_guest_send_file", job->rc);
            }
            break;
        }
        case AsyncOp::SelfLoopbackClipboardText: {
            char *out = nullptr;
            job->rc = lb_self_loopback_clipboard_text(job->text.c_str(), &out);
            if (job->rc != 0 || out == nullptr) {
                if (out) {
                    lb_string_free(out);
                }
                FillRcError(job->err, sizeof(job->err), "lb_self_loopback_clipboard_text", job->rc);
            } else {
                job->textOut = out;
                lb_string_free(out);
            }
            break;
        }
        case AsyncOp::HostRecvClipboardText: {
            char *out = nullptr;
            job->rc = lb_host_recv_clipboard_text(&out);
            if (job->rc != 0 || out == nullptr) {
                if (out) {
                    lb_string_free(out);
                }
                FillRcError(job->err, sizeof(job->err), "lb_host_recv_clipboard_text", job->rc);
            } else {
                job->textOut = out;
                lb_string_free(out);
            }
            break;
        }
        case AsyncOp::GuestSendClipboardText: {
            job->rc = lb_guest_send_clipboard_text(job->text.c_str());
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_guest_send_clipboard_text", job->rc);
            }
            break;
        }
        case AsyncOp::HostRecvClipboard: {
            uint8_t kind = 0;
            uint8_t *bytes = nullptr;
            size_t len = 0;
            job->rc = lb_host_recv_clipboard(&kind, &bytes, &len);
            if (job->rc != 0 || bytes == nullptr) {
                if (bytes) {
                    lb_bytes_free(bytes, len);
                }
                FillRcError(job->err, sizeof(job->err), "lb_host_recv_clipboard", job->rc);
            } else {
                job->clipKind = kind;
                job->bytesOutBuf.assign(bytes, bytes + len);
                lb_bytes_free(bytes, len);
            }
            break;
        }
        case AsyncOp::GuestSendClipboardPng: {
            job->rc = lb_guest_send_clipboard_png(job->pngIn.data(), job->pngIn.size());
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_guest_send_clipboard_png", job->rc);
            }
            break;
        }
        case AsyncOp::SelfLoopbackClipboardPng: {
            uint8_t *out = nullptr;
            size_t outLen = 0;
            job->rc = lb_self_loopback_clipboard_png(job->pngIn.data(), job->pngIn.size(), &out, &outLen);
            if (job->rc != 0 || out == nullptr) {
                if (out) {
                    lb_bytes_free(out, outLen);
                }
                FillRcError(job->err, sizeof(job->err), "lb_self_loopback_clipboard_png", job->rc);
            } else {
                job->bytesOutBuf.assign(out, out + outLen);
                lb_bytes_free(out, outLen);
            }
            break;
        }
        case AsyncOp::SessionGoOnline: {
            uint16_t outPort = 0;
            job->rc = lb_session_go_online(job->listenPort, &outPort);
            job->port = outPort;
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_session_go_online", job->rc);
            } else {
                char *json = nullptr;
                if (lb_session_status(&json) == 0 && json != nullptr) {
                    job->jsonOut = json;
                    lb_string_free(json);
                }
            }
            break;
        }
        case AsyncOp::SessionBeginPair: {
            char *peer = nullptr;
            job->rc = lb_session_begin_pair(&peer);
            if (peer) {
                job->textOut = peer;
                lb_string_free(peer);
            }
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_session_begin_pair", job->rc);
            } else {
                char *json = nullptr;
                if (lb_session_status(&json) == 0 && json != nullptr) {
                    job->jsonOut = json;
                    lb_string_free(json);
                }
            }
            break;
        }
        case AsyncOp::SessionPair: {
            job->rc = lb_session_pair(job->host.c_str(), job->port, job->code.c_str());
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_session_pair", job->rc);
            } else {
                char *json = nullptr;
                if (lb_session_status(&json) == 0 && json != nullptr) {
                    job->jsonOut = json;
                    lb_string_free(json);
                }
            }
            break;
        }
        case AsyncOp::SessionSendFile: {
            uint64_t n = 0;
            job->rc = lb_session_send_file(job->filePath.c_str(), job->remoteName.c_str(), &n);
            job->bytesOut = n;
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_session_send_file", job->rc);
            }
            break;
        }
        case AsyncOp::SessionSendClipboardText: {
            job->rc = lb_session_send_clipboard_text(job->text.c_str());
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_session_send_clipboard_text", job->rc);
            }
            break;
        }
        case AsyncOp::SessionSendClipboardPng: {
            job->rc = lb_session_send_clipboard_png(job->pngIn.data(), job->pngIn.size());
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_session_send_clipboard_png", job->rc);
            }
            break;
        }
        case AsyncOp::SessionServeOnce: {
            char *json = nullptr;
            job->rc = lb_session_serve_once(job->destRoot.c_str(), &json);
            if (job->rc != 0 || json == nullptr) {
                if (json) {
                    lb_string_free(json);
                }
                FillRcError(job->err, sizeof(job->err), "lb_session_serve_once", job->rc);
            } else {
                job->jsonOut = json;
                lb_string_free(json);
            }
            break;
        }
        case AsyncOp::SessionStatus: {
            char *json = nullptr;
            job->rc = lb_session_status(&json);
            if (job->rc != 0 || json == nullptr) {
                if (json) {
                    lb_string_free(json);
                }
                FillRcError(job->err, sizeof(job->err), "lb_session_status", job->rc);
            } else {
                job->jsonOut = json;
                lb_string_free(json);
            }
            break;
        }
        case AsyncOp::SessionProbePeer: {
            job->rc = lb_session_probe_peer();
            if (job->rc != 0) {
                FillRcError(job->err, sizeof(job->err), "lb_session_probe_peer", job->rc);
            } else {
                char *json = nullptr;
                if (lb_session_status(&json) == 0 && json != nullptr) {
                    job->jsonOut = json;
                    lb_string_free(json);
                }
            }
            break;
        }
        case AsyncOp::SessionUnpair: {
            // Bye + trust wipe block on the network, so keep it off the ArkTS UI thread.
            char *json = nullptr;
            job->rc = lb_session_unpair(&json);
            if (job->rc != 0 || json == nullptr) {
                if (json) {
                    lb_string_free(json);
                }
                FillRcError(job->err, sizeof(job->err), "lb_session_unpair", job->rc);
            } else {
                job->jsonOut = json;
                lb_string_free(json);
            }
            break;
        }
        default:
            break;
        }
}

static void AsyncComplete(napi_env env, napi_status status, void *data) {
    auto *job = reinterpret_cast<AsyncWorkData *>(data);
    if (status != napi_ok) {
        RejectWithMessage(env, job->deferred, "async work aborted");
        napi_delete_async_work(env, job->asyncWork);
        delete job;
        return;
    }
    if (job->rc != 0 || job->err[0] != '\0') {
        RejectWithMessage(env, job->deferred, job->err[0] ? job->err : "native failed");
        napi_delete_async_work(env, job->asyncWork);
        delete job;
        return;
    }

    napi_value result = nullptr;
    switch (job->op) {
        case AsyncOp::SelfLoopbackTransfer:
        case AsyncOp::GuestSendFile: {
            napi_create_double(env, static_cast<double>(job->bytesOut), &result);
            break;
        }
        case AsyncOp::HostRecvFile: {
            napi_create_object(env, &result);
            napi_value v_bytes = nullptr;
            napi_value v_path = nullptr;
            napi_create_double(env, static_cast<double>(job->bytesOut), &v_bytes);
            napi_create_string_utf8(env, job->pathOut.c_str(), NAPI_AUTO_LENGTH, &v_path);
            napi_set_named_property(env, result, "bytesTransferred", v_bytes);
            napi_set_named_property(env, result, "path", v_path);
            break;
        }
        case AsyncOp::HostAwaitPair:
        case AsyncOp::SelfLoopbackClipboardText:
        case AsyncOp::HostRecvClipboardText: {
            napi_create_string_utf8(env, job->textOut.c_str(), NAPI_AUTO_LENGTH, &result);
            break;
        }
        case AsyncOp::GuestPair:
        case AsyncOp::GuestSendClipboardText:
        case AsyncOp::GuestSendClipboardPng: {
            napi_get_undefined(env, &result);
            break;
        }
        case AsyncOp::HostRecvClipboard: {
            napi_create_object(env, &result);
            napi_value kindVal = nullptr;
            if (job->clipKind == 1) {
                napi_create_string_utf8(env, "text", NAPI_AUTO_LENGTH, &kindVal);
                napi_value textVal = nullptr;
                napi_create_string_utf8(env, reinterpret_cast<const char *>(job->bytesOutBuf.data()),
                                        job->bytesOutBuf.size(), &textVal);
                napi_set_named_property(env, result, "kind", kindVal);
                napi_set_named_property(env, result, "text", textVal);
            } else if (job->clipKind == 2) {
                napi_create_string_utf8(env, "png", NAPI_AUTO_LENGTH, &kindVal);
                void *abData = nullptr;
                napi_value ab = nullptr;
                napi_create_arraybuffer(env, job->bytesOutBuf.size(), &abData, &ab);
                if (abData && !job->bytesOutBuf.empty()) {
                    memcpy(abData, job->bytesOutBuf.data(), job->bytesOutBuf.size());
                }
                napi_set_named_property(env, result, "kind", kindVal);
                napi_set_named_property(env, result, "png", ab);
            } else {
                RejectWithMessage(env, job->deferred, "unknown clipboard kind");
                napi_delete_async_work(env, job->asyncWork);
                delete job;
                return;
            }
            break;
        }
        case AsyncOp::SelfLoopbackClipboardPng: {
            void *abData = nullptr;
            napi_create_arraybuffer(env, job->bytesOutBuf.size(), &abData, &result);
            if (abData && !job->bytesOutBuf.empty()) {
                memcpy(abData, job->bytesOutBuf.data(), job->bytesOutBuf.size());
            }
            break;
        }
        case AsyncOp::SessionGoOnline:
        case AsyncOp::SessionBeginPair:
        case AsyncOp::SessionPair:
        case AsyncOp::SessionServeOnce:
        case AsyncOp::SessionStatus:
        case AsyncOp::SessionProbePeer:
        case AsyncOp::SessionUnpair: {
            napi_create_string_utf8(env, job->jsonOut.c_str(), NAPI_AUTO_LENGTH, &result);
            break;
        }
        case AsyncOp::SessionSendFile: {
            napi_create_double(env, static_cast<double>(job->bytesOut), &result);
            break;
        }
        case AsyncOp::SessionSendClipboardText:
        case AsyncOp::SessionSendClipboardPng: {
            napi_get_undefined(env, &result);
            break;
        }
    }

    napi_resolve_deferred(env, job->deferred, result);
    napi_delete_async_work(env, job->asyncWork);
    delete job;
}

static napi_value QueueAsync(napi_env env, AsyncWorkData *job, const char *resourceName) {
    napi_value promise = nullptr;
    napi_deferred deferred = nullptr;
    napi_create_promise(env, &deferred, &promise);
    job->deferred = deferred;

    napi_value name = nullptr;
    napi_create_string_utf8(env, resourceName, NAPI_AUTO_LENGTH, &name);
    napi_create_async_work(env, nullptr, name, AsyncExecute, AsyncComplete, job, &job->asyncWork);
    napi_queue_async_work(env, job->asyncWork);
    return promise;
}

static napi_value ProtocolVersion(napi_env env, napi_callback_info /*info*/) {
    napi_value result;
    napi_create_uint32(env, lb_protocol_version(), &result);
    return result;
}

static napi_value FeaturesLabel(napi_env env, napi_callback_info /*info*/) {
    char *label = lb_features_label();
    napi_value result;
    if (label == nullptr) {
        napi_get_undefined(env, &result);
        return result;
    }
    napi_create_string_utf8(env, label, NAPI_AUTO_LENGTH, &result);
    lb_string_free(label);
    return result;
}

static napi_value MakePairingCode(napi_env env, napi_callback_info /*info*/) {
    char *code = lb_make_pairing_code();
    napi_value result;
    if (code == nullptr) {
        napi_get_undefined(env, &result);
        return result;
    }
    napi_create_string_utf8(env, code, NAPI_AUTO_LENGTH, &result);
    lb_string_free(code);
    return result;
}

static napi_value MakeIdentity(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);

    char aliasBuf[64] = "Phone";
    if (argc >= 1) {
        ReadUtf8(env, args[0], aliasBuf, sizeof(aliasBuf));
    }

    char *device_id = nullptr;
    char *alias = nullptr;
    char *fp = nullptr;
    int32_t rc = lb_make_identity(aliasBuf, &device_id, &alias, &fp);
    if (rc != 0 || device_id == nullptr || alias == nullptr || fp == nullptr) {
        if (device_id)
            lb_string_free(device_id);
        if (alias)
            lb_string_free(alias);
        if (fp)
            lb_string_free(fp);
        napi_throw_error(env, nullptr, "lb_make_identity failed");
        return nullptr;
    }

    napi_value obj;
    napi_create_object(env, &obj);
    napi_value v_id, v_alias, v_fp;
    napi_create_string_utf8(env, device_id, NAPI_AUTO_LENGTH, &v_id);
    napi_create_string_utf8(env, alias, NAPI_AUTO_LENGTH, &v_alias);
    napi_create_string_utf8(env, fp, NAPI_AUTO_LENGTH, &v_fp);
    napi_set_named_property(env, obj, "deviceId", v_id);
    napi_set_named_property(env, obj, "alias", v_alias);
    napi_set_named_property(env, obj, "fingerprintHex", v_fp);
    lb_string_free(device_id);
    lb_string_free(alias);
    lb_string_free(fp);
    return obj;
}

static napi_value SelfLoopbackTransfer(napi_env env, napi_callback_info /*info*/) {
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SelfLoopbackTransfer;
    return QueueAsync(env, job, "SelfLoopbackTransfer");
}

static napi_value HostStart(napi_env env, napi_callback_info info) {
    size_t argc = 2;
    napi_value args[2] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char dataDir[kMaxPathBytes] = "";
    char aliasBuf[64] = "Phone-host";
    if (argc >= 1) {
        ReadUtf8(env, args[0], dataDir, sizeof(dataDir));
    }
    if (argc >= 2) {
        ReadUtf8(env, args[1], aliasBuf, sizeof(aliasBuf));
    }
    if (dataDir[0] == '\0') {
        napi_throw_error(env, nullptr, "dataDir required");
        return nullptr;
    }
    uint16_t port = 0;
    char *code = nullptr;
    uint8_t paired = 0;
    int32_t rc = lb_host_start(dataDir, aliasBuf, &port, &code, &paired);
    if (rc != 0 || code == nullptr) {
        if (code)
            lb_string_free(code);
        char msg[128];
        FillRcError(msg, sizeof(msg), "lb_host_start", rc);
        napi_throw_error(env, nullptr, msg);
        return nullptr;
    }
    napi_value obj;
    napi_create_object(env, &obj);
    napi_value v_port, v_code, v_paired;
    napi_create_uint32(env, port, &v_port);
    napi_create_string_utf8(env, code, NAPI_AUTO_LENGTH, &v_code);
    napi_get_boolean(env, paired != 0, &v_paired);
    napi_set_named_property(env, obj, "port", v_port);
    napi_set_named_property(env, obj, "pairingCode", v_code);
    napi_set_named_property(env, obj, "paired", v_paired);
    lb_string_free(code);
    return obj;
}

static napi_value HostAwaitPair(napi_env env, napi_callback_info /*info*/) {
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::HostAwaitPair;
    return QueueAsync(env, job, "HostAwaitPair");
}

static napi_value HostStop(napi_env env, napi_callback_info /*info*/) {
    lb_host_stop();
    napi_value u;
    napi_get_undefined(env, &u);
    return u;
}

static napi_value HostRecvFile(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char dest[kMaxPathBytes] = "";
    if (argc >= 1) {
        ReadUtf8(env, args[0], dest, sizeof(dest));
    }
    if (dest[0] == '\0') {
        napi_throw_error(env, nullptr, "destRoot required");
        return nullptr;
    }
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::HostRecvFile;
    job->destRoot = dest;
    return QueueAsync(env, job, "HostRecvFile");
}

static napi_value GuestOpen(napi_env env, napi_callback_info info) {
    size_t argc = 2;
    napi_value args[2] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char dataDir[kMaxPathBytes] = "";
    char aliasBuf[64] = "Phone";
    if (argc >= 1) {
        ReadUtf8(env, args[0], dataDir, sizeof(dataDir));
    }
    if (argc >= 2) {
        ReadUtf8(env, args[1], aliasBuf, sizeof(aliasBuf));
    }
    if (dataDir[0] == '\0') {
        napi_throw_error(env, nullptr, "dataDir required");
        return nullptr;
    }
    uint8_t paired = 0;
    char *host = nullptr;
    uint16_t port = 0;
    char *peer = nullptr;
    int32_t rc = lb_guest_open(dataDir, aliasBuf, &paired, &host, &port, &peer);
    if (rc != 0) {
        if (host)
            lb_string_free(host);
        if (peer)
            lb_string_free(peer);
        char msg[128];
        FillRcError(msg, sizeof(msg), "lb_guest_open", rc);
        napi_throw_error(env, nullptr, msg);
        return nullptr;
    }
    napi_value obj;
    napi_create_object(env, &obj);
    napi_value v_paired, v_host, v_port, v_peer;
    napi_get_boolean(env, paired != 0, &v_paired);
    napi_create_string_utf8(env, host ? host : "", NAPI_AUTO_LENGTH, &v_host);
    napi_create_uint32(env, port, &v_port);
    napi_create_string_utf8(env, peer ? peer : "", NAPI_AUTO_LENGTH, &v_peer);
    napi_set_named_property(env, obj, "paired", v_paired);
    napi_set_named_property(env, obj, "host", v_host);
    napi_set_named_property(env, obj, "port", v_port);
    napi_set_named_property(env, obj, "peerId", v_peer);
    if (host)
        lb_string_free(host);
    if (peer)
        lb_string_free(peer);
    return obj;
}

static napi_value GuestPair(napi_env env, napi_callback_info info) {
    size_t argc = 3;
    napi_value args[3] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char host[128] = "";
    char code[32] = "";
    uint32_t port = 0;
    if (argc >= 1)
        ReadUtf8(env, args[0], host, sizeof(host));
    if (argc >= 2)
        napi_get_value_uint32(env, args[1], &port);
    if (argc >= 3)
        ReadUtf8(env, args[2], code, sizeof(code));
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::GuestPair;
    job->host = host;
    job->port = static_cast<uint16_t>(port);
    job->code = code;
    return QueueAsync(env, job, "GuestPair");
}

static napi_value GuestUpdateEndpoint(napi_env env, napi_callback_info info) {
    size_t argc = 2;
    napi_value args[2] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char host[128] = "";
    uint32_t port = 0;
    if (argc >= 1)
        ReadUtf8(env, args[0], host, sizeof(host));
    if (argc >= 2)
        napi_get_value_uint32(env, args[1], &port);
    int32_t rc = lb_guest_update_endpoint(host, static_cast<uint16_t>(port));
    if (rc != 0) {
        char msg[128];
        FillRcError(msg, sizeof(msg), "lb_guest_update_endpoint", rc);
        napi_throw_error(env, nullptr, msg);
        return nullptr;
    }
    napi_value u;
    napi_get_undefined(env, &u);
    return u;
}

static napi_value GuestSendFile(napi_env env, napi_callback_info info) {
    size_t argc = 2;
    napi_value args[2] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char filePath[kMaxPathBytes] = "";
    char remote[256] = "";
    if (argc >= 1)
        ReadUtf8(env, args[0], filePath, sizeof(filePath));
    if (argc >= 2)
        ReadUtf8(env, args[1], remote, sizeof(remote));
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::GuestSendFile;
    job->filePath = filePath;
    job->remoteName = remote;
    return QueueAsync(env, job, "GuestSendFile");
}

static napi_value SelfLoopbackClipboardText(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    std::string text = "lanbridge-clip-smoke";
    if (argc >= 1) {
        if (!ReadUtf8Owned(env, args[0], &text, kMaxClipTextBytes)) {
            napi_throw_error(env, nullptr, "clipboard text too long or invalid (max 1MiB)");
            return nullptr;
        }
        if (text.empty()) {
            text = "lanbridge-clip-smoke";
        }
    }
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SelfLoopbackClipboardText;
    job->text = std::move(text);
    return QueueAsync(env, job, "SelfLoopbackClipboardText");
}

static napi_value HostRecvClipboardText(napi_env env, napi_callback_info /*info*/) {
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::HostRecvClipboardText;
    return QueueAsync(env, job, "HostRecvClipboardText");
}

static napi_value GuestSendClipboardText(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    std::string text;
    if (argc >= 1) {
        if (!ReadUtf8Owned(env, args[0], &text, kMaxClipTextBytes)) {
            napi_throw_error(env, nullptr, "clipboard text too long or invalid (max 1MiB)");
            return nullptr;
        }
    }
    if (text.empty()) {
        napi_throw_error(env, nullptr, "text required");
        return nullptr;
    }
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::GuestSendClipboardText;
    job->text = std::move(text);
    return QueueAsync(env, job, "GuestSendClipboardText");
}

static napi_value HostRecvClipboard(napi_env env, napi_callback_info /*info*/) {
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::HostRecvClipboard;
    return QueueAsync(env, job, "HostRecvClipboard");
}

static napi_value GuestSendClipboardPng(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    if (argc < 1) {
        napi_throw_error(env, nullptr, "png ArrayBuffer required");
        return nullptr;
    }
    void *data = nullptr;
    size_t len = 0;
    if (napi_get_arraybuffer_info(env, args[0], &data, &len) != napi_ok || data == nullptr || len == 0) {
        napi_throw_error(env, nullptr, "invalid png ArrayBuffer");
        return nullptr;
    }
    if (len > kMaxClipPngBytes) {
        napi_throw_error(env, nullptr, "png too large (max 16MiB)");
        return nullptr;
    }
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::GuestSendClipboardPng;
    job->pngIn.assign(static_cast<uint8_t *>(data), static_cast<uint8_t *>(data) + len);
    return QueueAsync(env, job, "GuestSendClipboardPng");
}

static napi_value SelfLoopbackClipboardPng(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    if (argc < 1) {
        napi_throw_error(env, nullptr, "png ArrayBuffer required");
        return nullptr;
    }
    void *data = nullptr;
    size_t len = 0;
    if (napi_get_arraybuffer_info(env, args[0], &data, &len) != napi_ok || data == nullptr || len == 0) {
        napi_throw_error(env, nullptr, "invalid png ArrayBuffer");
        return nullptr;
    }
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SelfLoopbackClipboardPng;
    job->pngIn.assign(static_cast<uint8_t *>(data), static_cast<uint8_t *>(data) + len);
    return QueueAsync(env, job, "SelfLoopbackClipboardPng");
}

static napi_value SessionOpen(napi_env env, napi_callback_info info) {
    size_t argc = 3;
    napi_value args[3] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char dataDir[kMaxPathBytes] = "";
    char deviceId[64] = "lanbridge-ohos";
    char aliasBuf[64] = "Phone";
    if (argc >= 1) {
        ReadUtf8(env, args[0], dataDir, sizeof(dataDir));
    }
    if (argc >= 2) {
        ReadUtf8(env, args[1], deviceId, sizeof(deviceId));
    }
    if (argc >= 3) {
        ReadUtf8(env, args[2], aliasBuf, sizeof(aliasBuf));
    }
    if (dataDir[0] == '\0') {
        napi_throw_error(env, nullptr, "dataDir required");
        return nullptr;
    }
    char *json = nullptr;
    int32_t rc = lb_session_open(dataDir, deviceId, aliasBuf, &json);
    if (rc != 0 || json == nullptr) {
        if (json) {
            lb_string_free(json);
        }
        char msg[128];
        FillRcError(msg, sizeof(msg), "lb_session_open", rc);
        napi_throw_error(env, nullptr, msg);
        return nullptr;
    }
    napi_value result;
    napi_create_string_utf8(env, json, NAPI_AUTO_LENGTH, &result);
    lb_string_free(json);
    return result;
}

// Sync on ArkTS thread: lb_session_status only try_lock + atomics (never awaits the
// session mutex). Queuing it behind SessionSendFile/ServeOnce hid all % / rate UI.
static napi_value SessionStatus(napi_env env, napi_callback_info /*info*/) {
    char *json = nullptr;
    int32_t rc = lb_session_status(&json);
    if (rc != 0 || json == nullptr) {
        if (json) {
            lb_string_free(json);
        }
        char msg[128];
        FillRcError(msg, sizeof(msg), "lb_session_status", rc);
        napi_throw_error(env, nullptr, msg);
        return nullptr;
    }
    napi_value result;
    napi_create_string_utf8(env, json, NAPI_AUTO_LENGTH, &result);
    lb_string_free(json);
    return result;
}

static napi_value SessionGoOnline(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    uint32_t port = 0;
    if (argc >= 1) {
        napi_get_value_uint32(env, args[0], &port);
    }
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionGoOnline;
    job->listenPort = static_cast<uint16_t>(port);
    return QueueAsync(env, job, "SessionGoOnline");
}

static napi_value SessionGoOffline(napi_env env, napi_callback_info /*info*/) {
    lb_session_go_offline();
    char *json = nullptr;
    if (lb_session_status(&json) == 0 && json != nullptr) {
        napi_value result;
        napi_create_string_utf8(env, json, NAPI_AUTO_LENGTH, &result);
        lb_string_free(json);
        return result;
    }
    if (json) {
        lb_string_free(json);
    }
    napi_value u;
    napi_get_undefined(env, &u);
    return u;
}

static napi_value SessionUnpair(napi_env env, napi_callback_info /*info*/) {
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionUnpair;
    return QueueAsync(env, job, "SessionUnpair");
}

static napi_value SessionBeginPair(napi_env env, napi_callback_info /*info*/) {
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionBeginPair;
    return QueueAsync(env, job, "SessionBeginPair");
}

static napi_value SessionPair(napi_env env, napi_callback_info info) {
    size_t argc = 3;
    napi_value args[3] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionPair;
    if (argc >= 1) {
        ReadUtf8Owned(env, args[0], &job->host, 256);
    }
    uint32_t port = 37532;
    if (argc >= 2) {
        napi_get_value_uint32(env, args[1], &port);
    }
    job->port = static_cast<uint16_t>(port);
    if (argc >= 3) {
        ReadUtf8Owned(env, args[2], &job->code, 32);
    }
    return QueueAsync(env, job, "SessionPair");
}

static napi_value SessionSetPeer(napi_env env, napi_callback_info info) {
    size_t argc = 2;
    napi_value args[2] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    char host[256] = "";
    uint32_t port = 37532;
    if (argc >= 1) {
        ReadUtf8(env, args[0], host, sizeof(host));
    }
    if (argc >= 2) {
        napi_get_value_uint32(env, args[1], &port);
    }
    int32_t rc = lb_session_set_peer(host, static_cast<uint16_t>(port));
    if (rc != 0) {
        char msg[128];
        FillRcError(msg, sizeof(msg), "lb_session_set_peer", rc);
        napi_throw_error(env, nullptr, msg);
        return nullptr;
    }
    char *json = nullptr;
    if (lb_session_status(&json) == 0 && json != nullptr) {
        napi_value result;
        napi_create_string_utf8(env, json, NAPI_AUTO_LENGTH, &result);
        lb_string_free(json);
        return result;
    }
    if (json) {
        lb_string_free(json);
    }
    napi_value u;
    napi_get_undefined(env, &u);
    return u;
}

static napi_value SessionProbePeer(napi_env env, napi_callback_info /*info*/) {
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionProbePeer;
    return QueueAsync(env, job, "SessionProbePeer");
}

static napi_value SessionSendFile(napi_env env, napi_callback_info info) {
    size_t argc = 2;
    napi_value args[2] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionSendFile;
    if (argc >= 1) {
        ReadUtf8Owned(env, args[0], &job->filePath, kMaxPathBytes);
    }
    if (argc >= 2) {
        ReadUtf8Owned(env, args[1], &job->remoteName, 512);
    }
    return QueueAsync(env, job, "SessionSendFile");
}

static napi_value SessionSendClipboardText(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionSendClipboardText;
    if (argc >= 1) {
        ReadUtf8Owned(env, args[0], &job->text, kMaxClipTextBytes);
    }
    return QueueAsync(env, job, "SessionSendClipboardText");
}

static napi_value SessionSendClipboardPng(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    if (argc < 1) {
        napi_throw_error(env, nullptr, "png ArrayBuffer required");
        return nullptr;
    }
    void *data = nullptr;
    size_t len = 0;
    if (napi_get_arraybuffer_info(env, args[0], &data, &len) != napi_ok || data == nullptr || len == 0) {
        napi_throw_error(env, nullptr, "invalid png ArrayBuffer");
        return nullptr;
    }
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionSendClipboardPng;
    job->pngIn.assign(static_cast<uint8_t *>(data), static_cast<uint8_t *>(data) + len);
    return QueueAsync(env, job, "SessionSendClipboardPng");
}

static napi_value SessionServeOnce(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value args[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, args, nullptr, nullptr);
    auto *job = new AsyncWorkData();
    job->op = AsyncOp::SessionServeOnce;
    if (argc >= 1) {
        ReadUtf8Owned(env, args[0], &job->destRoot, kMaxPathBytes);
    }
    return QueueAsync(env, job, "SessionServeOnce");
}

static napi_value SessionPairingCode(napi_env env, napi_callback_info /*info*/) {
    char *code = nullptr;
    int32_t rc = lb_session_pairing_code(&code);
    if (rc != 0 || code == nullptr) {
        if (code) {
            lb_string_free(code);
        }
        char msg[128];
        FillRcError(msg, sizeof(msg), "lb_session_pairing_code", rc);
        napi_throw_error(env, nullptr, msg);
        return nullptr;
    }
    napi_value result;
    napi_create_string_utf8(env, code, NAPI_AUTO_LENGTH, &result);
    lb_string_free(code);
    return result;
}

EXTERN_C_START
static napi_value Init(napi_env env, napi_value exports) {
    napi_property_descriptor desc[] = {
        {"protocolVersion", nullptr, ProtocolVersion, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"featuresLabel", nullptr, FeaturesLabel, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"makePairingCode", nullptr, MakePairingCode, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"makeIdentity", nullptr, MakeIdentity, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"selfLoopbackTransfer", nullptr, SelfLoopbackTransfer, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"hostStart", nullptr, HostStart, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"hostAwaitPair", nullptr, HostAwaitPair, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"hostStop", nullptr, HostStop, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"hostRecvFile", nullptr, HostRecvFile, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"guestOpen", nullptr, GuestOpen, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"guestPair", nullptr, GuestPair, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"guestUpdateEndpoint", nullptr, GuestUpdateEndpoint, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"guestSendFile", nullptr, GuestSendFile, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"selfLoopbackClipboardText", nullptr, SelfLoopbackClipboardText, nullptr, nullptr, nullptr, napi_default,
         nullptr},
        {"hostRecvClipboardText", nullptr, HostRecvClipboardText, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"guestSendClipboardText", nullptr, GuestSendClipboardText, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"hostRecvClipboard", nullptr, HostRecvClipboard, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"guestSendClipboardPng", nullptr, GuestSendClipboardPng, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"selfLoopbackClipboardPng", nullptr, SelfLoopbackClipboardPng, nullptr, nullptr, nullptr, napi_default,
         nullptr},
        {"sessionOpen", nullptr, SessionOpen, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionStatus", nullptr, SessionStatus, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionGoOnline", nullptr, SessionGoOnline, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionGoOffline", nullptr, SessionGoOffline, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionUnpair", nullptr, SessionUnpair, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionBeginPair", nullptr, SessionBeginPair, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionPair", nullptr, SessionPair, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionSetPeer", nullptr, SessionSetPeer, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionProbePeer", nullptr, SessionProbePeer, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionSendFile", nullptr, SessionSendFile, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionSendClipboardText", nullptr, SessionSendClipboardText, nullptr, nullptr, nullptr, napi_default,
         nullptr},
        {"sessionSendClipboardPng", nullptr, SessionSendClipboardPng, nullptr, nullptr, nullptr, napi_default,
         nullptr},
        {"sessionServeOnce", nullptr, SessionServeOnce, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"sessionPairingCode", nullptr, SessionPairingCode, nullptr, nullptr, nullptr, napi_default, nullptr},
    };
    napi_define_properties(env, exports, sizeof(desc) / sizeof(desc[0]), desc);
    return exports;
}
EXTERN_C_END

static napi_module lbNapiModule = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "lb_napi",
    .nm_priv = nullptr,
    .reserved = {0},
};

extern "C" __attribute__((constructor)) void RegisterLbNapiModule(void) {
    napi_module_register(&lbNapiModule);
}
