package io.amux.app.ui

import android.webkit.CookieManager
import android.webkit.WebView
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import io.amux.app.ServerManager

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
fun ContentScreen(serverUrl: String, serverManager: ServerManager) {
    var isLoading by remember { mutableStateOf(false) }
    var canGoBack by remember { mutableStateOf(false) }
    var showSettings by remember { mutableStateOf(false) }
    var webView by remember { mutableStateOf<WebView?>(null) }

    // System back button → WebView back
    BackHandler(enabled = canGoBack) {
        webView?.goBack()
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .combinedClickable(
                onClick = {},
                onLongClick = { showSettings = true }
            )
    ) {
        // WebView — use AndroidView directly for ref access
        AndroidView(
            factory = { context ->
                WebView(context).apply {
                    setBackgroundColor(0xFF0D1117.toInt())

                    settings.apply {
                        javaScriptEnabled = true
                        domStorageEnabled = true
                        databaseEnabled = true
                        mediaPlaybackRequiresUserGesture = false
                        setSupportMultipleWindows(true)
                        javaScriptCanOpenWindowsAutomatically = true
                        userAgentString = "$userAgentString AmuxApp"
                    }

                    CookieManager.getInstance().setAcceptCookie(true)
                    CookieManager.getInstance().setAcceptThirdPartyCookies(this, true)

                    webViewClient = object : android.webkit.WebViewClient() {
                        override fun onReceivedSslError(
                            view: WebView,
                            handler: android.webkit.SslErrorHandler,
                            error: android.net.http.SslError
                        ) {
                            val host = error.url?.let { android.net.Uri.parse(it).host } ?: ""
                            val isTrusted = host == "localhost" ||
                                    host.endsWith(".ts.net") ||
                                    host.endsWith(".local")
                            if (isTrusted) handler.proceed() else handler.cancel()
                        }

                        override fun shouldOverrideUrlLoading(
                            view: WebView,
                            request: android.webkit.WebResourceRequest
                        ): Boolean {
                            val requestHost = request.url.host ?: return false
                            val serverHost = android.net.Uri.parse(serverUrl).host ?: return false
                            if (request.isForMainFrame && requestHost != serverHost) {
                                context.startActivity(
                                    android.content.Intent(
                                        android.content.Intent.ACTION_VIEW,
                                        request.url
                                    )
                                )
                                return true
                            }
                            return false
                        }

                        override fun onPageStarted(
                            view: WebView, url: String?, favicon: android.graphics.Bitmap?
                        ) {
                            isLoading = true
                        }

                        override fun onPageFinished(view: WebView, url: String?) {
                            isLoading = false
                            canGoBack = view.canGoBack()
                        }
                    }

                    webChromeClient = object : android.webkit.WebChromeClient() {
                        override fun onCreateWindow(
                            view: WebView, isDialog: Boolean,
                            isUserGesture: Boolean, resultMsg: android.os.Message?
                        ): Boolean {
                            val transport =
                                resultMsg?.obj as? WebView.WebViewTransport ?: return false
                            val tempView = WebView(view.context)
                            tempView.webViewClient = object : android.webkit.WebViewClient() {
                                override fun shouldOverrideUrlLoading(
                                    v: WebView,
                                    request: android.webkit.WebResourceRequest
                                ): Boolean {
                                    view.loadUrl(request.url.toString())
                                    return true
                                }
                            }
                            transport.webView = tempView
                            resultMsg.sendToTarget()
                            return true
                        }
                    }

                    loadUrl(serverUrl)
                    webView = this
                }
            },
            update = { wv ->
                val currentHost = android.net.Uri.parse(wv.url ?: "").host
                val newHost = android.net.Uri.parse(serverUrl).host
                if (currentHost != null && newHost != null && currentHost != newHost) {
                    wv.loadUrl(serverUrl)
                }
            },
            modifier = Modifier.fillMaxSize()
        )

        // Loading indicator
        if (isLoading) {
            LinearProgressIndicator(
                modifier = Modifier
                    .fillMaxWidth()
                    .align(Alignment.TopCenter),
                color = MaterialTheme.colorScheme.primary
            )
        }
    }

    // Settings bottom sheet
    if (showSettings) {
        SettingsSheet(
            serverManager = serverManager,
            currentUrl = serverUrl,
            onDismiss = { showSettings = false }
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsSheet(
    serverManager: ServerManager,
    currentUrl: String,
    onDismiss: () -> Unit
) {
    val servers by serverManager.servers.collectAsState()
    var showAddServer by remember { mutableStateOf(false) }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = MaterialTheme.colorScheme.surface
    ) {
        Column(modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            Text(
                "Servers",
                fontSize = 18.sp,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.padding(bottom = 12.dp)
            )

            servers.forEachIndexed { index, server ->
                val isCurrent = server.url.trimEnd('/') == currentUrl.trimEnd('/')
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(server.name, fontWeight = FontWeight.SemiBold, fontSize = 15.sp)
                        Text(
                            server.url,
                            fontSize = 12.sp,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                    if (isCurrent) {
                        Text(
                            "current",
                            fontSize = 12.sp,
                            color = MaterialTheme.colorScheme.primary
                        )
                    } else {
                        Row {
                            TextButton(onClick = {
                                serverManager.selectServer(server.url)
                                onDismiss()
                            }) {
                                Text("Open", fontSize = 12.sp)
                            }
                            TextButton(onClick = { serverManager.removeServer(index) }) {
                                Text("✕", fontSize = 12.sp)
                            }
                        }
                    }
                }
            }

            if (servers.isEmpty()) {
                Text(
                    "No servers saved.",
                    fontSize = 13.sp,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(vertical = 8.dp)
                )
            }

            Spacer(Modifier.height(12.dp))

            // Add server
            if (showAddServer) {
                AddServerForm(
                    onAdd = { name, url ->
                        if (serverManager.addServer(name, url)) {
                            showAddServer = false
                        }
                    },
                    onCancel = { showAddServer = false }
                )
            } else {
                OutlinedButton(
                    onClick = { showAddServer = true },
                    modifier = Modifier.fillMaxWidth(),
                    shape = RoundedCornerShape(10.dp)
                ) {
                    Text("Add Server")
                }
            }

            Spacer(Modifier.height(8.dp))

            TextButton(
                onClick = {
                    serverManager.resetServer()
                    onDismiss()
                },
                modifier = Modifier.fillMaxWidth()
            ) {
                Text("Back to Setup", color = MaterialTheme.colorScheme.error)
            }

            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
fun AddServerForm(onAdd: (String, String) -> Unit, onCancel: () -> Unit) {
    var name by remember { mutableStateOf("") }
    var url by remember { mutableStateOf("") }
    var error by remember { mutableStateOf(false) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
    ) {
        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            label = { Text("Name (optional)") },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
            shape = RoundedCornerShape(10.dp)
        )
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = url,
            onValueChange = { url = it; error = false },
            label = { Text("URL") },
            placeholder = { Text("https://amux.tail-xxxx.ts.net:8824") },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
            shape = RoundedCornerShape(10.dp),
            keyboardOptions = KeyboardOptions(
                keyboardType = KeyboardType.Uri,
                imeAction = ImeAction.Done
            ),
            keyboardActions = KeyboardActions(onDone = {
                onAdd(name.ifBlank { url }, url)
            }),
            isError = error
        )
        if (error) {
            Text(
                "Invalid URL — must start with http:// or https://",
                fontSize = 12.sp,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(top = 4.dp)
            )
        }
        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = onCancel, modifier = Modifier.weight(1f)) {
                Text("Cancel")
            }
            Button(
                onClick = {
                    val n = name.ifBlank { url }
                    if (!url.startsWith("http://") && !url.startsWith("https://")) {
                        error = true
                    } else {
                        onAdd(n, url)
                    }
                },
                modifier = Modifier.weight(1f),
                enabled = url.isNotBlank()
            ) {
                Text("Add")
            }
        }
    }
}
