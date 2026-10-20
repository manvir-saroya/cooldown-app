package app.cooldown.phone

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Log
import java.security.GeneralSecurityException
import java.security.InvalidKeyException
import java.security.KeyStore
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** A failure with a message that is safe and useful to show the user. Never carries key material. */
class KeyWrapException(message: String) : Exception(message)

/**
 * Wraps the phone's 32-byte identity key with a non-exportable AES-256-GCM key in the AndroidKeyStore
 * (pairing-protocol.md section 2.2: the Keystore cannot do X25519, so the raw key lives in Rust memory
 * and only its encrypted blob is stored).
 *
 * Blob format: `version (1 byte, = 1) || IV (12 bytes) || ciphertext || GCM tag (16 bytes)`.
 * The version byte is also bound in as additional authenticated data.
 *
 * The wrapping key is created on first use, StrongBox-backed when the device has one, otherwise in
 * the TEE or software-backed Keystore. It needs no user authentication, so background sync works with
 * the phone locked. It is deleted by Android when the app is uninstalled, together with the blob.
 */
object KeyWrap {
  const val ALIAS = "cooldown-identity-wrap"
  const val KEY_BYTES = 32
  private const val VERSION: Byte = 1
  private const val IV_BYTES = 12
  private const val TAG_BITS = 128
  private const val TAG_BYTES = TAG_BITS / 8
  private const val TRANSFORMATION = "AES/GCM/NoPadding"
  private const val PROVIDER = "AndroidKeyStore"
  private const val TAG = "CooldownKeyWrap"

  /** Encrypt [raw] (exactly 32 bytes). The caller zeroes [raw]. */
  fun wrap(raw: ByteArray): ByteArray {
    if (raw.size != KEY_BYTES) throw KeyWrapException("The key to protect must be exactly 32 bytes.")
    try {
      return try {
        encrypt(getOrCreateKey(), raw)
      } catch (e: InvalidKeyException) {
        // The Keystore entry exists but is unusable (for example invalidated): make a fresh one.
        // Blobs wrapped by the old key are lost either way.
        Log.w(TAG, "Wrapping key unusable (${e.javaClass.simpleName}), creating a new one")
        deleteKey()
        encrypt(getOrCreateKey(), raw)
      }
    } catch (e: GeneralSecurityException) {
      throw KeyWrapException("The Android Keystore could not protect the key (${e.javaClass.simpleName}).")
    } catch (e: RuntimeException) {
      throw KeyWrapException("The Android Keystore could not protect the key (${e.javaClass.simpleName}).")
    }
  }

  /** Decrypt a blob made by [wrap]. The caller zeroes the returned array. Never creates a key. */
  fun unwrap(blob: ByteArray): ByteArray {
    if (blob.size < 1 + IV_BYTES + TAG_BYTES) {
      throw KeyWrapException("The stored key is damaged (too short).")
    }
    if (blob[0] != VERSION) {
      throw KeyWrapException("The stored key has an unknown format (version ${blob[0].toInt() and 0xff}).")
    }
    val key = existingKey()
      ?: throw KeyWrapException("The device's protection key is missing, so the stored key cannot be opened.")
    var plain: ByteArray? = null
    try {
      val cipher = Cipher.getInstance(TRANSFORMATION)
      cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, blob, 1, IV_BYTES))
      cipher.updateAAD(byteArrayOf(VERSION))
      plain = cipher.doFinal(blob, 1 + IV_BYTES, blob.size - 1 - IV_BYTES)
      if (plain.size != KEY_BYTES) {
        throw KeyWrapException("The stored key is damaged (wrong length).")
      }
      val out = plain
      plain = null
      return out
    } catch (e: AEADBadTagException) {
      throw KeyWrapException("The stored key is damaged or was made by a different install.")
    } catch (e: GeneralSecurityException) {
      throw KeyWrapException("The Android Keystore could not open the stored key (${e.javaClass.simpleName}).")
    } catch (e: RuntimeException) {
      throw KeyWrapException("The Android Keystore could not open the stored key (${e.javaClass.simpleName}).")
    } finally {
      plain?.fill(0)
    }
  }

  private fun encrypt(key: SecretKey, raw: ByteArray): ByteArray {
    val cipher = Cipher.getInstance(TRANSFORMATION)
    cipher.init(Cipher.ENCRYPT_MODE, key) // the Keystore picks a fresh random IV
    cipher.updateAAD(byteArrayOf(VERSION))
    val iv = cipher.iv
    if (iv == null || iv.size != IV_BYTES) throw KeyWrapException("The Android Keystore gave an unexpected IV.")
    val sealed = cipher.doFinal(raw)
    val out = ByteArray(1 + IV_BYTES + sealed.size)
    out[0] = VERSION
    System.arraycopy(iv, 0, out, 1, IV_BYTES)
    System.arraycopy(sealed, 0, out, 1 + IV_BYTES, sealed.size)
    return out
  }

  private fun keyStore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

  private fun existingKey(): SecretKey? = synchronized(this) { keyStore().getKey(ALIAS, null) as? SecretKey }

  private fun deleteKey() = synchronized(this) {
    val store = keyStore()
    if (store.containsAlias(ALIAS)) store.deleteEntry(ALIAS)
  }

  private fun getOrCreateKey(): SecretKey = synchronized(this) {
    existingKey() ?: createKey()
  }

  private fun createKey(): SecretKey {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      try {
        val key = generate(strongBox = true)
        Log.i(TAG, "Wrapping key created in StrongBox")
        return key
      } catch (e: Exception) {
        // No StrongBox on this device (StrongBoxUnavailableException) or it refused: fall back silently.
        Log.i(TAG, "StrongBox not available (${e.javaClass.simpleName}), using the standard Keystore")
      }
    }
    val key = generate(strongBox = false)
    Log.i(TAG, "Wrapping key created in the standard Keystore")
    return key
  }

  private fun generate(strongBox: Boolean): SecretKey {
    val spec = KeyGenParameterSpec.Builder(
      ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
    )
      .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
      .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
      .setKeySize(256)
      .setRandomizedEncryptionRequired(true)
      .apply {
        if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) setIsStrongBoxBacked(true)
      }
      .build()
    val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
    generator.init(spec)
    return generator.generateKey()
  }
}
