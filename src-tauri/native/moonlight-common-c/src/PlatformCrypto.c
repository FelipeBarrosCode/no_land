#include "Limelight-internal.h"

#ifdef USE_MBEDTLS
#include <mbedtls/entropy.h>
#include <mbedtls/ctr_drbg.h>
#include <mbedtls/version.h>

mbedtls_entropy_context EntropyContext;
mbedtls_ctr_drbg_context CtrDrbgContext;
bool RandomStateInitialized = false;

#if MBEDTLS_VERSION_MAJOR > 2 || (MBEDTLS_VERSION_MAJOR == 2 && MBEDTLS_VERSION_MINOR >= 25)
#define USE_MBEDTLS_CRYPTO_EXT
#endif

#elif defined(_WIN32)
#include <windows.h>
#include <bcrypt.h>

#else
#include <openssl/evp.h>
#include <openssl/rand.h>
#endif

static int addPkcs7PaddingInPlace(unsigned char* plaintext, int plaintextLen) {
    int paddedLength = ROUND_TO_PKCS7_PADDED_LEN(plaintextLen);
    unsigned char paddingByte = (unsigned char)(16 - (plaintextLen % 16));

    memset(&plaintext[plaintextLen], paddingByte, paddedLength - plaintextLen);

    return paddedLength;
}

#if defined(_WIN32) && !defined(USE_MBEDTLS)
static bool windows_set_chaining_mode(BCRYPT_ALG_HANDLE algHandle, LPCWSTR chainingMode) {
    ULONG modeSize = (ULONG)((wcslen(chainingMode) + 1) * sizeof(WCHAR));
    return BCryptSetProperty(algHandle, BCRYPT_CHAINING_MODE, (PUCHAR)chainingMode, modeSize, 0) == 0;
}

static bool windows_open_aes_algorithm(int algorithm, BCRYPT_ALG_HANDLE* algHandle) {
    if (BCryptOpenAlgorithmProvider(algHandle, BCRYPT_AES_ALGORITHM, NULL, 0) != 0) {
        return false;
    }

    if (algorithm == ALGORITHM_AES_GCM) {
        if (!windows_set_chaining_mode(*algHandle, BCRYPT_CHAIN_MODE_GCM)) {
            BCryptCloseAlgorithmProvider(*algHandle, 0);
            *algHandle = NULL;
            return false;
        }
    }
    else if (algorithm == ALGORITHM_AES_CBC) {
        if (!windows_set_chaining_mode(*algHandle, BCRYPT_CHAIN_MODE_CBC)) {
            BCryptCloseAlgorithmProvider(*algHandle, 0);
            *algHandle = NULL;
            return false;
        }
    }
    else {
        BCryptCloseAlgorithmProvider(*algHandle, 0);
        *algHandle = NULL;
        return false;
    }

    return true;
}

static bool windows_generate_aes_key(BCRYPT_ALG_HANDLE algHandle,
                                     unsigned char* key,
                                     int keyLength,
                                     BCRYPT_KEY_HANDLE* keyHandle,
                                     PUCHAR* keyObject,
                                     DWORD* keyObjectLength) {
    DWORD resultSize = 0;
    if (BCryptGetProperty(algHandle, BCRYPT_OBJECT_LENGTH, (PUCHAR)keyObjectLength,
                          sizeof(*keyObjectLength), &resultSize, 0) != 0) {
        return false;
    }

    *keyObject = (PUCHAR)malloc(*keyObjectLength);
    if (*keyObject == NULL) {
        return false;
    }

    if (BCryptGenerateSymmetricKey(algHandle, keyHandle, *keyObject, *keyObjectLength,
                                   key, (ULONG)keyLength, 0) != 0) {
        free(*keyObject);
        *keyObject = NULL;
        return false;
    }

    return true;
}

static void windows_cleanup_aes(BCRYPT_ALG_HANDLE algHandle,
                                BCRYPT_KEY_HANDLE keyHandle,
                                PUCHAR keyObject) {
    if (keyHandle != NULL) {
        BCryptDestroyKey(keyHandle);
    }
    free(keyObject);
    if (algHandle != NULL) {
        BCryptCloseAlgorithmProvider(algHandle, 0);
    }
}

static bool windows_remove_pkcs7_padding(unsigned char* plaintext, int* plaintextLen) {
    if (plaintext == NULL || plaintextLen == NULL || *plaintextLen <= 0) {
        return false;
    }

    unsigned char padding = plaintext[*plaintextLen - 1];
    if (padding == 0 || padding > 16 || padding > *plaintextLen) {
        return false;
    }

    for (int i = 0; i < padding; i++) {
        if (plaintext[*plaintextLen - 1 - i] != padding) {
            return false;
        }
    }

    *plaintextLen -= padding;
    return true;
}
#endif

// When CIPHER_FLAG_PAD_TO_BLOCK_SIZE is used, inputData buffer must be allocated such that
// the buffer length is at least ROUND_TO_PKCS7_PADDED_LEN(inputDataLength) and inputData
// buffer may be modified!
// For GCM, the IV can change from message to message without CIPHER_FLAG_RESET_IV.
// CIPHER_FLAG_RESET_IV is only required for GCM when the IV length changes.
// Changing the key between encrypt/decrypt calls on a single context is not supported.
bool PltEncryptMessage(PPLT_CRYPTO_CONTEXT ctx, int algorithm, int flags,
                       unsigned char* key, int keyLength,
                       unsigned char* iv, int ivLength,
                       unsigned char* tag, int tagLength,
                       unsigned char* inputData, int inputDataLength,
                       unsigned char* outputData, int* outputDataLength) {
#ifdef USE_MBEDTLS
    mbedtls_cipher_mode_t cipherMode;
    size_t outLength;

    switch (algorithm) {
    case ALGORITHM_AES_CBC:
        LC_ASSERT(tag == NULL);
        LC_ASSERT(tagLength == 0);
        cipherMode = MBEDTLS_MODE_CBC;
        break;
    case ALGORITHM_AES_GCM:
        LC_ASSERT(tag != NULL);
        LC_ASSERT(tagLength > 0);
        cipherMode = MBEDTLS_MODE_GCM;
        break;
    default:
        LC_ASSERT(false);
        return false;
    }

    if (!ctx->initialized) {
        if (mbedtls_cipher_setup(&ctx->ctx, mbedtls_cipher_info_from_values(MBEDTLS_CIPHER_ID_AES, keyLength * 8, cipherMode)) != 0) {
            return false;
        }

        if (mbedtls_cipher_setkey(&ctx->ctx, key, keyLength * 8, MBEDTLS_ENCRYPT) != 0) {
            return false;
        }

        ctx->initialized = true;
    }

    if (tag != NULL) {
#ifdef USE_MBEDTLS_CRYPTO_EXT
        // In mbedTLS, tag is always after ciphertext, while we need to put tag BEFORE ciphertext here
        // To avoid frequent heap allocation, we will use some evil tricks...
        // We only support 16 bytes sized tag
        LC_ASSERT(tagLength == 16);
        // Assume outputData is right after tag
        LC_ASSERT(outputData == tag + tagLength);
#ifndef LC_DEBUG
        if (tagLength != 16 || outputData != tag + tagLength) {
            return false;
        }
#endif
        size_t encryptedLength = 0;
        unsigned char * encryptedData = tag;
        size_t encryptedCapacity = inputDataLength + tagLength;
        if (mbedtls_cipher_auth_encrypt_ext(&ctx->ctx, iv, ivLength, NULL, 0, inputData, inputDataLength, encryptedData,
                                            encryptedCapacity, &encryptedLength, tagLength) != 0) {
            return false;
        }
        outLength = encryptedLength - tagLength;

        unsigned char tagTemp[16];
        // Copy the tag to temp buffer
        memcpy(tagTemp, encryptedData + outLength, tagLength);
        // Move ciphertext to the end
        memmove(encryptedData + tagLength, encryptedData, outLength);
        // Copy back tag
        memcpy(encryptedData, tagTemp, tagLength);
#else
        if (mbedtls_cipher_auth_encrypt(&ctx->ctx, iv, ivLength, NULL, 0, inputData, inputDataLength, outputData, &outLength, tag, tagLength) != 0) {
            return false;
        }
#endif
    }
    else {
        if (flags & CIPHER_FLAG_RESET_IV) {
            if (mbedtls_cipher_set_iv(&ctx->ctx, iv, ivLength) != 0) {
                return false;
            }

            mbedtls_cipher_reset(&ctx->ctx);
        }

        if (flags & CIPHER_FLAG_PAD_TO_BLOCK_SIZE) {
            inputDataLength = addPkcs7PaddingInPlace(inputData, inputDataLength);
        }

        if (mbedtls_cipher_update(&ctx->ctx, inputData, inputDataLength, outputData, &outLength) != 0) {
            return false;
        }

        if (flags & CIPHER_FLAG_FINISH) {
            size_t finishLength;

            if (mbedtls_cipher_finish(&ctx->ctx, &outputData[outLength], &finishLength) != 0) {
                return false;
            }

            outLength += finishLength;
        }
    }

    *outputDataLength = outLength;
    return true;
#elif defined(_WIN32)
    LC_ASSERT(keyLength == 16);

    BCRYPT_ALG_HANDLE algHandle = NULL;
    BCRYPT_KEY_HANDLE keyHandle = NULL;
    PUCHAR keyObject = NULL;
    DWORD keyObjectLength = 0;
    ULONG resultLength = 0;
    NTSTATUS cryptoStatus = (NTSTATUS)-1;
    bool success = false;

    if (!windows_open_aes_algorithm(algorithm, &algHandle)) {
        return false;
    }
    if (!windows_generate_aes_key(algHandle, key, keyLength, &keyHandle, &keyObject, &keyObjectLength)) {
        windows_cleanup_aes(algHandle, keyHandle, keyObject);
        return false;
    }

    if (algorithm == ALGORITHM_AES_GCM) {
        BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO authInfo;
        BCRYPT_INIT_AUTH_MODE_INFO(authInfo);
        authInfo.pbNonce = iv;
        authInfo.cbNonce = (ULONG)ivLength;
        authInfo.pbTag = tag;
        authInfo.cbTag = (ULONG)tagLength;

        cryptoStatus = BCryptEncrypt(keyHandle,
                                     inputData,
                                     (ULONG)inputDataLength,
                                     &authInfo,
                                     NULL,
                                     0,
                                     outputData,
                                     (ULONG)inputDataLength,
                                     &resultLength,
                                     0);
        success = cryptoStatus == 0;
    }
    else if (algorithm == ALGORITHM_AES_CBC) {
        unsigned char ivCopy[16];
        LC_ASSERT(ivLength <= (int)sizeof(ivCopy));
        if (ivLength > (int)sizeof(ivCopy)) {
            windows_cleanup_aes(algHandle, keyHandle, keyObject);
            return false;
        }
        memcpy(ivCopy, iv, (size_t)ivLength);

        if (flags & CIPHER_FLAG_PAD_TO_BLOCK_SIZE) {
            inputDataLength = addPkcs7PaddingInPlace(inputData, inputDataLength);
        }

        cryptoStatus = BCryptEncrypt(keyHandle,
                                     inputData,
                                     (ULONG)inputDataLength,
                                     NULL,
                                     ivCopy,
                                     (ULONG)ivLength,
                                     outputData,
                                     (ULONG)inputDataLength,
                                     &resultLength,
                                     0);
        success = cryptoStatus == 0;
    }
    else {
        LC_ASSERT(false);
        windows_cleanup_aes(algHandle, keyHandle, keyObject);
        return false;
    }

    if (success) {
        *outputDataLength = (int)resultLength;
    }
    else {
        Limelog("BCryptEncrypt failed: 0x%08X\n", (unsigned int)cryptoStatus);
    }

    windows_cleanup_aes(algHandle, keyHandle, keyObject);
    return success;
#else
    LC_ASSERT(keyLength == 16);

    if (algorithm == ALGORITHM_AES_GCM) {
        LC_ASSERT(tag != NULL);
        LC_ASSERT(tagLength > 0);

        if (!ctx->initialized || (flags & CIPHER_FLAG_RESET_IV)) {
            if (EVP_EncryptInit_ex(ctx->ctx, EVP_aes_128_gcm(), NULL, NULL, NULL) != 1) {
                return false;
            }

            if (EVP_CIPHER_CTX_ctrl(ctx->ctx, EVP_CTRL_GCM_SET_IVLEN, ivLength, NULL) != 1) {
                return false;
            }

            if (EVP_EncryptInit_ex(ctx->ctx, NULL, NULL, key, iv) != 1) {
                return false;
            }

            ctx->initialized = true;
        }
        else {
            if (EVP_EncryptInit_ex(ctx->ctx, NULL, NULL, NULL, iv) != 1) {
                return false;
            }
        }
    }
    else if (algorithm == ALGORITHM_AES_CBC) {
        LC_ASSERT(tag == NULL);
        LC_ASSERT(tagLength == 0);

        if (!ctx->initialized) {
            if (EVP_EncryptInit_ex(ctx->ctx, EVP_aes_128_cbc(), NULL, key, iv) != 1) {
                return false;
            }

            ctx->initialized = true;
        }
        else if (flags & CIPHER_FLAG_RESET_IV) {
            if (EVP_EncryptInit_ex(ctx->ctx, NULL, NULL, NULL, iv) != 1) {
                return false;
            }
        }

        if (flags & CIPHER_FLAG_PAD_TO_BLOCK_SIZE) {
            inputDataLength = addPkcs7PaddingInPlace(inputData, inputDataLength);
        }
    }
    else {
        LC_ASSERT(false);
        return false;
    }

    if (EVP_EncryptUpdate(ctx->ctx, outputData, outputDataLength, inputData, inputDataLength) != 1) {
        return false;
    }

    if (algorithm == ALGORITHM_AES_GCM) {
        int len;

        if (EVP_EncryptFinal_ex(ctx->ctx, outputData, &len) != 1) {
            return false;
        }
        LC_ASSERT(len == 0);

        if (EVP_CIPHER_CTX_ctrl(ctx->ctx, EVP_CTRL_GCM_GET_TAG, tagLength, tag) != 1) {
            return false;
        }
    }
    else if (flags & CIPHER_FLAG_FINISH) {
        int len;

        if (EVP_EncryptFinal_ex(ctx->ctx, &outputData[*outputDataLength], &len) != 1) {
            return false;
        }

        *outputDataLength += len;
    }

    return true;
#endif
}

// When CBC is used, outputData buffer must be allocated such that the buffer length is
// at least ROUND_TO_PKCS7_PADDED_LEN(inputDataLength) to allow room for PKCS7 padding.
// For GCM, the IV can change from message to message without CIPHER_FLAG_RESET_IV.
// CIPHER_FLAG_RESET_IV is only required for GCM when the IV length changes.
// Changing the key between encrypt/decrypt calls on a single context is not supported.
bool PltDecryptMessage(PPLT_CRYPTO_CONTEXT ctx, int algorithm, int flags,
                       unsigned char* key, int keyLength,
                       unsigned char* iv, int ivLength,
                       unsigned char* tag, int tagLength,
                       unsigned char* inputData, int inputDataLength,
                       unsigned char* outputData, int* outputDataLength) {
#ifdef USE_MBEDTLS
    mbedtls_cipher_mode_t cipherMode;
    size_t outLength;

    switch (algorithm) {
    case ALGORITHM_AES_CBC:
        LC_ASSERT(tag == NULL);
        LC_ASSERT(tagLength == 0);
        cipherMode = MBEDTLS_MODE_CBC;
        break;
    case ALGORITHM_AES_GCM:
        LC_ASSERT(tag != NULL);
        LC_ASSERT(tagLength > 0);
        cipherMode = MBEDTLS_MODE_GCM;
        break;
    default:
        LC_ASSERT(false);
        return false;
    }

    if (!ctx->initialized) {
        if (mbedtls_cipher_setup(&ctx->ctx, mbedtls_cipher_info_from_values(MBEDTLS_CIPHER_ID_AES, keyLength * 8, cipherMode)) != 0) {
            return false;
        }

        if (mbedtls_cipher_setkey(&ctx->ctx, key, keyLength * 8, MBEDTLS_DECRYPT) != 0) {
            return false;
        }

        ctx->initialized = true;
    }

    if (tag != NULL) {
#ifdef USE_MBEDTLS_CRYPTO_EXT
        // We only support 16 bytes sized tag
        LC_ASSERT(tagLength == 16);
        // Assume inputData is right after tag
        LC_ASSERT(inputData == tag + tagLength);
#ifndef LC_DEBUG
        if (tagLength != 16 || inputData != tag + tagLength) {
            return false;
        }
#endif
        unsigned char * encryptedData = tag;
        size_t encryptedDataLen = inputDataLength + tagLength;
        unsigned char tagTemp[16];
        // Copy the tag to temp buffer
        memcpy(tagTemp, encryptedData, tagLength);
        // Move ciphertext to the beginning
        memmove(encryptedData, encryptedData + tagLength, inputDataLength);
        // Copy back tag to the end
        memcpy(encryptedData + inputDataLength, tagTemp, tagLength);
        if (mbedtls_cipher_auth_decrypt_ext(&ctx->ctx, iv, ivLength, NULL, 0, encryptedData, encryptedDataLen,
                                            outputData, inputDataLength, &outLength, tagLength) != 0) {
            return false;
        }
#else
        if (mbedtls_cipher_auth_decrypt(&ctx->ctx, iv, ivLength, NULL, 0, inputData, inputDataLength, outputData, &outLength, tag, tagLength) != 0) {
            return false;
        }
#endif
    }
    else {
        if (flags & CIPHER_FLAG_RESET_IV) {
            if (mbedtls_cipher_set_iv(&ctx->ctx, iv, ivLength) != 0) {
                return false;
            }

            mbedtls_cipher_reset(&ctx->ctx);
        }

        if (mbedtls_cipher_update(&ctx->ctx, inputData, inputDataLength, outputData, &outLength) != 0) {
            return false;
        }

        if (flags & CIPHER_FLAG_FINISH) {
            size_t finishLength;

            if (mbedtls_cipher_finish(&ctx->ctx, &outputData[outLength], &finishLength) != 0) {
                return false;
            }

            outLength += finishLength;
        }
    }

    *outputDataLength = outLength;
    return true;
#elif defined(_WIN32)
    LC_ASSERT(keyLength == 16);

    BCRYPT_ALG_HANDLE algHandle = NULL;
    BCRYPT_KEY_HANDLE keyHandle = NULL;
    PUCHAR keyObject = NULL;
    DWORD keyObjectLength = 0;
    ULONG resultLength = 0;
    bool success = false;

    if (!windows_open_aes_algorithm(algorithm, &algHandle)) {
        return false;
    }
    if (!windows_generate_aes_key(algHandle, key, keyLength, &keyHandle, &keyObject, &keyObjectLength)) {
        windows_cleanup_aes(algHandle, keyHandle, keyObject);
        return false;
    }

    if (algorithm == ALGORITHM_AES_GCM) {
        BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO authInfo;
        BCRYPT_INIT_AUTH_MODE_INFO(authInfo);
        authInfo.pbNonce = iv;
        authInfo.cbNonce = (ULONG)ivLength;
        authInfo.pbTag = tag;
        authInfo.cbTag = (ULONG)tagLength;

        success = BCryptDecrypt(keyHandle,
                                inputData,
                                (ULONG)inputDataLength,
                                &authInfo,
                                NULL,
                                0,
                                outputData,
                                (ULONG)inputDataLength,
                                &resultLength,
                                0) == 0;
    }
    else if (algorithm == ALGORITHM_AES_CBC) {
        unsigned char ivCopy[16];
        LC_ASSERT(ivLength <= (int)sizeof(ivCopy));
        if (ivLength > (int)sizeof(ivCopy)) {
            windows_cleanup_aes(algHandle, keyHandle, keyObject);
            return false;
        }
        memcpy(ivCopy, iv, (size_t)ivLength);

        success = BCryptDecrypt(keyHandle,
                                inputData,
                                (ULONG)inputDataLength,
                                NULL,
                                ivCopy,
                                (ULONG)ivLength,
                                outputData,
                                (ULONG)inputDataLength,
                                &resultLength,
                                0) == 0;
        if (success && (flags & CIPHER_FLAG_FINISH)) {
            int plaintextLength = (int)resultLength;
            success = windows_remove_pkcs7_padding(outputData, &plaintextLength);
            resultLength = (ULONG)plaintextLength;
        }
    }
    else {
        LC_ASSERT(false);
        windows_cleanup_aes(algHandle, keyHandle, keyObject);
        return false;
    }

    if (success) {
        *outputDataLength = (int)resultLength;
    }

    windows_cleanup_aes(algHandle, keyHandle, keyObject);
    return success;
#else
    LC_ASSERT(keyLength == 16);

    if (algorithm == ALGORITHM_AES_GCM) {
        LC_ASSERT(tag != NULL);
        LC_ASSERT(tagLength > 0);

        if (!ctx->initialized || (flags & CIPHER_FLAG_RESET_IV)) {
            if (EVP_DecryptInit_ex(ctx->ctx, EVP_aes_128_gcm(), NULL, NULL, NULL) != 1) {
                return false;
            }

            if (EVP_CIPHER_CTX_ctrl(ctx->ctx, EVP_CTRL_GCM_SET_IVLEN, ivLength, NULL) != 1) {
                return false;
            }

            if (EVP_DecryptInit_ex(ctx->ctx, NULL, NULL, key, iv) != 1) {
                return false;
            }

            ctx->initialized = true;
        }
        else {
            if (EVP_DecryptInit_ex(ctx->ctx, NULL, NULL, NULL, iv) != 1) {
                return false;
            }
        }
    }
    else if (algorithm == ALGORITHM_AES_CBC) {
        LC_ASSERT(tag == NULL);
        LC_ASSERT(tagLength == 0);

        if (!ctx->initialized) {
            if (EVP_DecryptInit_ex(ctx->ctx, EVP_aes_128_cbc(), NULL, key, iv) != 1) {
                return false;
            }

            ctx->initialized = true;
        }
        else if (flags & CIPHER_FLAG_RESET_IV) {
            if (EVP_DecryptInit_ex(ctx->ctx, NULL, NULL, NULL, iv) != 1) {
                return false;
            }
        }
    }
    else {
        LC_ASSERT(false);
        return false;
    }

    if (EVP_DecryptUpdate(ctx->ctx, outputData, outputDataLength, inputData, inputDataLength) != 1) {
        return false;
    }

    if (algorithm == ALGORITHM_AES_GCM) {
        int len;

        if (EVP_CIPHER_CTX_ctrl(ctx->ctx, EVP_CTRL_GCM_SET_TAG, tagLength, tag) != 1) {
            return false;
        }

        if (EVP_DecryptFinal_ex(ctx->ctx, outputData, &len) != 1) {
            return false;
        }
        LC_ASSERT(len == 0);
    }
    else if (flags & CIPHER_FLAG_FINISH) {
        int len;

        if (EVP_DecryptFinal_ex(ctx->ctx, &outputData[*outputDataLength], &len) != 1) {
            return false;
        }

        *outputDataLength += len;
    }

    return true;
#endif
}

PPLT_CRYPTO_CONTEXT PltCreateCryptoContext(void) {
    PPLT_CRYPTO_CONTEXT ctx = malloc(sizeof(*ctx));
    if (!ctx) {
        return NULL;
    }

    ctx->initialized = false;

#ifdef USE_MBEDTLS
    mbedtls_cipher_init(&ctx->ctx);
#elif defined(_WIN32)
    ctx->ctx = NULL;
#else
    ctx->ctx = EVP_CIPHER_CTX_new();
    if (!ctx->ctx) {
        free(ctx);
        return NULL;
    }
#endif

    return ctx;
}

void PltDestroyCryptoContext(PPLT_CRYPTO_CONTEXT ctx) {
#ifdef USE_MBEDTLS
    mbedtls_cipher_free(&ctx->ctx);
#elif defined(_WIN32)
    (void)ctx;
#else
    EVP_CIPHER_CTX_free(ctx->ctx);
#endif
    free(ctx);
}

void PltGenerateRandomData(unsigned char* data, int length) {
#ifdef USE_MBEDTLS
    // FIXME: This is not thread safe...
    if (!RandomStateInitialized) {
        mbedtls_entropy_init(&EntropyContext);
        mbedtls_ctr_drbg_init(&CtrDrbgContext);
        if (mbedtls_ctr_drbg_seed(&CtrDrbgContext, mbedtls_entropy_func, &EntropyContext, NULL, 0) != 0) {
            Limelog("Seeding MbedTLS random number generator failed!\n");
            LC_ASSERT(false);
            return;
        }

        RandomStateInitialized = true;
    }

    mbedtls_ctr_drbg_random(&CtrDrbgContext, data, length);
#elif defined(_WIN32)
    BCryptGenRandom(NULL, data, (ULONG)length, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
#else
    RAND_bytes(data, length);
#endif
}
