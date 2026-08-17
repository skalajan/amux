package io.amux.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.amux.app.ServerManager

@Composable
fun ServerPickerScreen(serverManager: ServerManager) {
    var customUrl by remember { mutableStateOf("") }
    var customName by remember { mutableStateOf("") }
    var urlError by remember { mutableStateOf(false) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(horizontal = 20.dp),
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Spacer(Modifier.height(80.dp))

        // Header
        Text(
            "amux",
            fontSize = 36.sp,
            fontWeight = FontWeight.Bold,
            color = Color.White
        )
        Spacer(Modifier.height(12.dp))
        Text(
            "Connect to your amux server",
            fontSize = 14.sp,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Spacer(Modifier.height(36.dp))

        // Cloud option
        Button(
            onClick = {
                serverManager.addServer("amux cloud", "https://cloud.amux.io")
                serverManager.selectServer("https://cloud.amux.io")
            },
            modifier = Modifier.fillMaxWidth(),
            shape = RoundedCornerShape(12.dp),
            contentPadding = PaddingValues(14.dp)
        ) {
            Text("☁  Sign in to amux cloud", fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        }

        Spacer(Modifier.height(12.dp))
        Text(
            "Includes Sign in with Apple & Google",
            fontSize = 12.sp,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )

        Spacer(Modifier.height(24.dp))

        // Divider
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth()
        ) {
            HorizontalDivider(Modifier.weight(1f), color = MaterialTheme.colorScheme.outline)
            Text(
                "  or self-hosted  ",
                fontSize = 12.sp,
                color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f)
            )
            HorizontalDivider(Modifier.weight(1f), color = MaterialTheme.colorScheme.outline)
        }

        Spacer(Modifier.height(24.dp))

        // Server URL form
        OutlinedTextField(
            value = customName,
            onValueChange = { customName = it },
            label = { Text("Name (optional)") },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
            shape = RoundedCornerShape(10.dp)
        )

        Spacer(Modifier.height(8.dp))

        OutlinedTextField(
            value = customUrl,
            onValueChange = { customUrl = it; urlError = false },
            label = { Text("https://amux.tail-xxxx.ts.net:8824") },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
            shape = RoundedCornerShape(10.dp),
            keyboardOptions = KeyboardOptions(
                keyboardType = KeyboardType.Uri,
                imeAction = ImeAction.Go
            ),
            keyboardActions = KeyboardActions(onGo = {
                val name = customName.ifBlank { customUrl }
                if (serverManager.addServer(name, customUrl)) {
                    serverManager.selectServer(customUrl)
                } else {
                    urlError = true
                }
            }),
            isError = urlError
        )

        Spacer(Modifier.height(4.dp))
        Text(
            "Find your Tailscale hostname in the Tailscale app. Port is 8824 by default.",
            fontSize = 12.sp,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 4.dp)
        )

        if (urlError) {
            Spacer(Modifier.height(8.dp))
            Text(
                "Please enter a valid URL starting with http:// or https://",
                fontSize = 12.sp,
                color = MaterialTheme.colorScheme.error
            )
        }

        Spacer(Modifier.height(16.dp))

        OutlinedButton(
            onClick = {
                urlError = false
                val name = customName.ifBlank { customUrl }
                if (serverManager.addServer(name, customUrl)) {
                    serverManager.selectServer(customUrl)
                } else {
                    urlError = true
                }
            },
            modifier = Modifier.fillMaxWidth(),
            shape = RoundedCornerShape(12.dp),
            contentPadding = PaddingValues(14.dp),
            enabled = customUrl.isNotBlank()
        ) {
            Text("Connect", fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        }
    }
}
