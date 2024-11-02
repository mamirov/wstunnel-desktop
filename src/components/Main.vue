<script lang="ts">
import {defineComponent, ref} from 'vue'
import ClientOptionsEditor from "./ClientOptionsEditor.vue";

export default defineComponent({
  name: "Main",
  components: {ClientOptionsEditor}
})
</script>

<template>
  <v-layout>

    <v-app-bar
        color="teal-darken-4">
      <v-app-bar-title>Wstunnel Client</v-app-bar-title>

      <v-spacer></v-spacer>

      <v-btn icon>
        <v-icon>mdi-text</v-icon>
        <v-tooltip
            activator="parent"
            location="bottom"
        >Logs
        </v-tooltip>
      </v-btn>

    </v-app-bar>

    <v-navigation-drawer permanent>
      <v-list>
        <v-list-item>
          <v-btn color="primary">Add new conf</v-btn>
        </v-list-item>
        <v-divider></v-divider>
      </v-list>
    </v-navigation-drawer>

    <v-main class="d-flex align-center justify-center" style="min-height: 300px;">
      Main Content
    </v-main>

  </v-layout>
</template>

<style scoped>
</style>